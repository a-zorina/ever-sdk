use std::convert::TryInto;
use std::future::Future;
use std::ops::Range;
use std::sync::Arc;

use failure::{bail, err_msg};
use serde_json::Value;
use ton_block::{BlkPrevInfo, Deserializable, ShardStateUnsplit};
use ton_types::{Result, UInt256};

use crate::boc::internal::get_boc_hash;
use crate::client::NetworkUID;
use crate::ClientContext;
use crate::error::ClientResult;
use crate::net::{OrderBy, ParamsOfQueryCollection, query_collection, SortDirection};
use crate::net::types::TrustedMcBlockId;
use crate::proofs::{BlockProof, get_current_network_uid, ProofHelperEngine, resolve_initial_trusted_key_block, storage::ProofStorage};

const ZEROSTATE_KEY: &str = "zerostate";
const ZEROSTATE_RIGHT_BOUND_KEY: &str = "zs_right_boundary_seq_no";

pub struct ProofHelperEngineImpl<Storage: ProofStorage + Send + Sync> {
    context: Arc<ClientContext>,
    storage: Arc<Storage>,
}

impl<Storage: ProofStorage + Send + Sync> ProofHelperEngineImpl<Storage> {
    pub fn new(context: Arc<ClientContext>, storage: Arc<Storage>) -> Self {
        Self { context, storage }
    }

    fn gen_root_hash_prefix(root_hash: &str) -> &str {
        &root_hash[..std::cmp::min(8, root_hash.len())]
    }

    fn gen_storage_key(network_uid: &NetworkUID, key: &str) -> String {
        format!(
            "{}/{}/{}",
            Self::gen_root_hash_prefix(&network_uid.zerostate_root_hash),
            Self::gen_root_hash_prefix(&network_uid.first_master_block_root_hash),
            key,
        )
    }

    fn gen_trusted_block_left_bound_key(seq_no: u32) -> String {
        format!("trusted_{}_left_boundary_seq_no", seq_no)
    }

    fn gen_trusted_block_right_bound_key(seq_no: u32) -> String {
        format!("trusted_{}_right_boundary_seq_no", seq_no)
    }

    async fn read_storage(&self, key: &str) -> ClientResult<Option<Vec<u8>>> {
        let network_uid = get_current_network_uid(&self.context).await?;
        let key = Self::gen_storage_key(&network_uid, key);

        self.storage.get(&key).await
    }

    async fn write_storage(&self, key: &str, value: &[u8]) -> ClientResult<()> {
        let network_uid = get_current_network_uid(&self.context).await?;
        let key = Self::gen_storage_key(&network_uid, key);

        self.storage.put(&key, value).await
    }

    async fn read_metadata_value_u32(&self, key: &str) -> ClientResult<Option<u32>> {
        Ok(
            self.read_storage(key).await?
                .map(|vec|
                    vec.try_into()
                        .ok()
                        .map(|arr| u32::from_le_bytes(arr))
                ).flatten()
        )
    }

    async fn write_metadata_value_u32(&self, key: &str, value: u32) -> ClientResult<()> {
        self.write_storage(key, &value.to_le_bytes()).await
    }

    async fn update_metadata_value_u32(
        &self,
        key: &str,
        value: u32,
        process_value: fn(u32, u32) -> u32,
    ) -> ClientResult<()> {
        match self.read_metadata_value_u32(key).await? {
            None => self.write_metadata_value_u32(key, value).await,
            Some(prev) => self.write_metadata_value_u32(key, process_value(prev, value)).await,
        }
    }

    async fn read_zs_right_bound(&self) -> ClientResult<u32> {
        self.read_metadata_value_u32(ZEROSTATE_RIGHT_BOUND_KEY).await
            .map(|opt| opt.unwrap_or(0))
    }

    async fn update_zs_right_bound(&self, seq_no: u32) -> ClientResult<()> {
        self.update_metadata_value_u32(ZEROSTATE_RIGHT_BOUND_KEY, seq_no, std::cmp::max).await
    }

    async fn read_trusted_block_left_bound(&self, trusted_seq_no: u32) -> ClientResult<u32> {
        self.read_metadata_value_u32(&Self::gen_trusted_block_left_bound_key(trusted_seq_no)).await
            .map(|opt| opt.unwrap_or(trusted_seq_no))
    }

    async fn update_trusted_block_left_bound(
        &self,
        trusted_seq_no: u32,
        left_bound_seq_no: u32,
    ) -> ClientResult<()> {
        self.update_metadata_value_u32(
            &Self::gen_trusted_block_left_bound_key(trusted_seq_no),
            left_bound_seq_no,
            std::cmp::min,
        ).await
    }

    async fn read_trusted_block_right_bound(&self, trusted_seq_no: u32) -> ClientResult<u32> {
        self.read_metadata_value_u32(&Self::gen_trusted_block_right_bound_key(trusted_seq_no)).await
            .map(|opt| opt.unwrap_or(trusted_seq_no))
    }

    async fn update_trusted_block_right_bound(
        &self,
        trusted_seq_no: u32,
        right_bound_seq_no: u32,
    ) -> ClientResult<()> {
        self.update_metadata_value_u32(
            &Self::gen_trusted_block_right_bound_key(trusted_seq_no),
            right_bound_seq_no,
            std::cmp::max,
        ).await
    }

    async fn query_zerostate_boc(&self) -> Result<Vec<u8>> {
        let zerostates = query_collection(
            Arc::clone(&self.context),
            ParamsOfQueryCollection {
                collection: "zerostates".to_string(),
                result: "boc".to_string(),
                limit: Some(1),
                ..Default::default()
            }
        ).await?.result;

        if zerostates.is_empty() {
            bail!("Unable to download network's zerostate from DApp server");
        }

        let boc = zerostates[0]["boc"].as_str()
            .ok_or_else(|| err_msg("BoC of zerostate must be a string"))?;

        Ok(base64::decode(boc)?)
    }

    async fn query_proof_boc_ext(&self, filter: Value) -> Result<Option<Vec<u8>>> {
        let blocks = query_collection(
            Arc::clone(&self.context),
            ParamsOfQueryCollection {
                collection: "blocks".to_string(),
                result: "signatures{proof}".to_string(),
                filter: Some(filter),
                limit: Some(1),
                ..Default::default()
            }
        ).await?.result;

        if blocks.is_empty() {
            return Ok(None);
        }

        let boc = blocks[0]["signatures"]["proof"].as_str()
            .ok_or_else(|| err_msg("BoC of proof must be a string"))?;

        Ok(Some(base64::decode(boc)?))
    }

    async fn query_proof_boc(
        &self,
        workchain_id: i32,
        shard: &str,
        seq_no: u32,
    ) -> Result<Vec<u8>> {
        let boc_opt = self.query_proof_boc_ext(
            json!({
                "workchain_id": {
                    "eq": workchain_id,
                },
                "shard": {
                    "eq": shard,
                },
                "seq_no": {
                    "eq": seq_no,
                }
            })
        ).await?;

        match boc_opt {
            Some(boc) => Ok(boc),

            None => bail!(
                "Unable to download proof for block [{}, {}, {}] from DApp server",
                workchain_id,
                shard,
                seq_no,
            ),
        }
    }

    async fn query_mc_proof_boc(&self, mc_seq_no: u32) -> Result<Vec<u8>> {
        let boc_opt = self.query_proof_boc_ext(
            json!({
                "workchain_id": {
                    "eq": -1,
                },
                "seq_no": {
                    "eq": mc_seq_no,
                }
            })
        ).await?;

        match boc_opt {
            Some(boc) => Ok(boc),

            None => bail!(
                "Unable to download proof for masterchain block with seq_no: {} from DApp server",
                mc_seq_no,
            ),
        }
    }

    async fn query_key_blocks_proofs_boc(
        &self,
        mut mc_seq_no_range: Range<u32>,
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        let mut result = Vec::new();
        loop {
            if mc_seq_no_range.is_empty() {
                return Ok(result);
            }

            let key_blocks = query_collection(
                Arc::clone(&self.context),
                ParamsOfQueryCollection {
                    collection: "blocks".to_string(),
                    result: "seq_no, signatures{proof}".to_string(),
                    filter: Some(json!({
                    "workchain_id": {
                        "eq": -1,
                    },
                    "key_block": {
                        "eq": true,
                    },
                    "seq_no": {
                        "ge": mc_seq_no_range.start,
                        "lt": mc_seq_no_range.end,
                    }
                })),
                    order: Some(
                        vec![OrderBy {
                            path: "seq_no".to_string(),
                            direction: SortDirection::ASC,
                        }]
                    ),
                    ..Default::default()
                }
            ).await?.result;

            if key_blocks.is_empty() {
                return Ok(result);
            }

            for key_block in key_blocks {
                let seq_no = key_block["seq_no"].as_u64()
                    .ok_or_else(|| err_msg("seq_no of block must be an integer value"))? as u32;
                let boc = key_block["signatures"]["proof"].as_str()
                    .ok_or_else(|| err_msg("BoC of proof must be a string"))?;
                result.push((seq_no, base64::decode(boc)?));
                mc_seq_no_range.start = seq_no + 1;
            }
        }
    }

    async fn query_blocks_proofs_boc(
        &self,
        mut seq_numbers_sorted: &[u32],
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        let mut result = Vec::new();
        while seq_numbers_sorted.len() > 0 {
            let blocks = query_collection(
                Arc::clone(&self.context),
                ParamsOfQueryCollection {
                    collection: "blocks".to_string(),
                    result: "seq_no, signatures{proof}".to_string(),
                    filter: Some(json!({
                        "workchain_id": {
                            "eq": -1,
                        },
                        "seq_no": {
                            "in": seq_numbers_sorted,
                        }
                    })),
                    order: Some(
                        vec![OrderBy {
                            path: "seq_no".to_string(),
                            direction: SortDirection::ASC,
                        }]
                    ),
                    ..Default::default()
                }
            ).await?.result;

            if seq_numbers_sorted.len() < blocks.len() {
                bail!(
                    "DApp server returned more blocks ({}) than expected ({})",
                    blocks.len(),
                    seq_numbers_sorted.len(),
                )
            }

            let (expected, remaining) = seq_numbers_sorted.split_at(blocks.len());
            for i in 0..blocks.len() {
                let block = &blocks[i];
                let seq_no = block["seq_no"].as_u64()
                    .ok_or_else(|| err_msg("seq_no of block must be an integer value"))? as u32;

                if seq_no != expected[i] {
                    bail!("Block with seq_no: {} missed on DApp server", expected[i]);
                }

                let boc = block["signatures"]["proof"].as_str()
                    .ok_or_else(|| err_msg("BoC of proof must be a string"))?;
                result.push((seq_no, base64::decode(boc)?));
            }

            seq_numbers_sorted = remaining;
        }

        Ok(result)
    }

    async fn download_trusted_key_block_proof(
        &self,
        id: &TrustedMcBlockId,
    ) -> Result<BlockProof> {
        let boc = self.query_mc_proof_boc(id.seq_no).await?;
        let proof =  BlockProof::deserialize(&boc)?;
        if proof.id().seq_no() != id.seq_no {
            bail!(
                    "Proof for trusted key-block seq_no ({}) mismatches trusted key-block seq_no ({})",
                    proof.id().seq_no,
                    id.seq_no,
                );
        }
        let trusted_root_hash = UInt256::from_str(&id.root_hash)?;
        if proof.id().root_hash() != trusted_root_hash {
            bail!(
                    "Proof for trusted key-block root_hash ({:?}) mismatches trusted key-block root_hash ({:?})",
                    proof.id().root_hash(),
                    trusted_root_hash,
                )
        }
        self.write_storage(&Self::make_mc_proof_key(id.seq_no), &boc).await?;

        Ok(proof)
    }

    async fn require_trusted_key_block_proof(
        &self,
        id: &TrustedMcBlockId,
    ) -> Result<BlockProof> {
        if let Some(boc) = self.read_storage(&Self::make_mc_proof_key(id.seq_no)).await? {
            return BlockProof::deserialize(&boc);
        }

        self.download_trusted_key_block_proof(id).await
    }

    fn make_mc_proof_key(mc_seq_no: u32) -> String {
        format!("proof_mc_{}", mc_seq_no)
    }

    async fn download_proof_chain<F: Fn(u32) -> R, R: Future<Output = ClientResult<()>>>(
        &self,
        mc_seq_no_range: Range<u32>,
        on_store_block: F,
    ) -> Result<BlockProof> {
        if mc_seq_no_range.is_empty() {
            bail!("Empty materchain seq_no range");
        }

        let proof_bocs = self.query_key_blocks_proofs_boc(mc_seq_no_range).await?;

        let mut last_proof = None;
        for (mc_seq_no, boc) in proof_bocs {
            let proof = BlockProof::deserialize(&boc)?;
            proof.check_proof(self).await?;

            self.write_storage(&Self::make_mc_proof_key(mc_seq_no), &boc).await?;
            on_store_block(mc_seq_no).await?;

            last_proof = Some(proof);
        }

        last_proof.ok_or_else(|| err_msg("Empty proof chain"))
    }

    async fn download_proof_chain_backward(
        &self,
        mc_seq_no_range: Range<u32>,
        trusted_seq_no: u32,
    ) -> Result<BlockProof> {
        if mc_seq_no_range.is_empty() {
            bail!("Empty materchain seq_no range");
        }

        let key_proof_bocs = self.query_key_blocks_proofs_boc(mc_seq_no_range.clone()).await?;

        let next_seq_no_sorted: Vec<u32> = key_proof_bocs.iter()
            .map(|(mc_seq_no, _boc)| mc_seq_no + 1)
            .collect();

        let next_blocks_proof_bocs = self.query_blocks_proofs_boc(&next_seq_no_sorted).await?;

        let right_key_proof_boc = self.read_storage(&Self::make_mc_proof_key(mc_seq_no_range.end)).await?
            .ok_or_else(
                || err_msg(format!("Cannot load proof for MC seq_no: {}", mc_seq_no_range.end))
            )?;
        let mut right_key_proof = BlockProof::deserialize(&right_key_proof_boc)?;
        for ((key_seq_no, key_boc), (_next_seq_no, next_boc))
            in key_proof_bocs.iter().zip(next_blocks_proof_bocs.iter()).rev()
        {
            let key_proof = BlockProof::deserialize(&key_boc)?;
            key_proof.pre_check_block_proof()?;

            let next_block_proof = BlockProof::deserialize(&next_boc)?;
            let (next_block, next_block_info) = next_block_proof.pre_check_block_proof()?;
            if next_block_info.prev_key_block_seqno() != *key_seq_no {
                bail!("Block is expected to be just after its key-block");
            }
            next_block_proof.check_with_prev_key_block_proof_(
                &key_proof,
                &next_block,
                &next_block_info,
            )?;
            let prev_root_hash = match next_block_info.read_prev_ref()? {
                BlkPrevInfo::Block { prev } => prev.root_hash,
                BlkPrevInfo::Blocks { .. } => bail!("Unexpected merge in masterchain"),
            };

            let key_root_hash = key_proof.id().root_hash();
            if prev_root_hash != key_root_hash {
                bail!(
                    "Proof chain is broken: next block's prev root_hash ({:?}) links to an incorrect \
                        root_hash ({:?})",
                    prev_root_hash,
                    key_root_hash,
                );
            }

            right_key_proof.check_with_prev_key_block_proof(&key_proof)?;

            self.write_storage(&Self::make_mc_proof_key(*key_seq_no), &key_boc).await?;
            self.update_trusted_block_left_bound(trusted_seq_no, *key_seq_no).await?;

            right_key_proof = key_proof;
        }

        Ok(right_key_proof)
    }
}

#[async_trait::async_trait]
impl<Storage: ProofStorage + Send + Sync> ProofHelperEngine for ProofHelperEngineImpl<Storage> {
    fn context(&self) -> &Arc<ClientContext> {
        &self.context
    }

    async fn load_zerostate(&self) -> Result<ShardStateUnsplit> {
        if let Some(boc) = self.read_storage(ZEROSTATE_KEY).await? {
            return ShardStateUnsplit::construct_from_bytes(&boc);
        }

        let boc = self.query_zerostate_boc().await?;

        let actual_hash = get_boc_hash(&boc)?;
        let expected_hash = &get_current_network_uid(self.context()).await?
            .zerostate_root_hash;
        if actual_hash != *expected_hash {
            bail!(
                "Zerostate hashes mismatch (expected `{}`, but queried from DApp is `{}`)",
                expected_hash,
                actual_hash,
            );
        }

        self.write_storage(ZEROSTATE_KEY, &boc).await?;

        ShardStateUnsplit::construct_from_bytes(&boc)
    }

    async fn load_key_block_proof(&self, mc_seq_no: u32) -> Result<BlockProof> {
        let proof_key = Self::make_mc_proof_key(mc_seq_no);
        if let Some(boc) = self.read_storage(&proof_key).await? {
            return BlockProof::deserialize(&boc);
        }
        
        let trusted_id = resolve_initial_trusted_key_block(self.context()).await?;
        let zs_right_bound = self.read_zs_right_bound().await?;
        let trusted_left_bound = self.read_trusted_block_left_bound(trusted_id.seq_no).await?;
        let trusted_right_bound = self.read_trusted_block_right_bound(trusted_id.seq_no).await?;

        if mc_seq_no == trusted_id.seq_no {
            return self.download_trusted_key_block_proof(trusted_id).await;
        }

        self.require_trusted_key_block_proof(trusted_id).await?;

        let update_zs_right = move |mc_seq_no| async move {
            self.update_zs_right_bound(mc_seq_no).await
        };

        let update_trusted_right = move |mc_seq_no| async move {
            self.update_trusted_block_right_bound(trusted_id.seq_no, mc_seq_no).await
        };

        if mc_seq_no > trusted_right_bound {
            self.download_proof_chain(trusted_right_bound..mc_seq_no + 1, update_trusted_right).await
        } else if mc_seq_no < zs_right_bound + (trusted_left_bound - zs_right_bound) / 2 {
            self.download_proof_chain(zs_right_bound + 1..mc_seq_no + 1, update_zs_right).await
        } else if mc_seq_no < trusted_left_bound {
            self.download_proof_chain_backward(mc_seq_no..trusted_left_bound, trusted_id.seq_no).await
        } else if mc_seq_no <= zs_right_bound {
            // Chain from zerostate is broken
            self.download_proof_chain(1..mc_seq_no + 1, update_zs_right).await
        } else if mc_seq_no >= trusted_left_bound && mc_seq_no <= trusted_id.seq_no {
            // Chain from trusted key-block to the left is broken
            self.download_proof_chain_backward(mc_seq_no..trusted_id.seq_no, trusted_id.seq_no).await
        } else if mc_seq_no > trusted_id.seq_no && mc_seq_no <= trusted_right_bound {
            // Chain from trusted key-block to the right is broken
            self.download_proof_chain(trusted_id.seq_no + 1..mc_seq_no + 1, update_trusted_right).await
        } else {
            unreachable!(
                "mc_seq_no: {}, zs_right: {}, trusted_left: {}, trusted_right: {}, trusted_id: {:?}",
                mc_seq_no,
                zs_right_bound,
                trusted_left_bound,
                trusted_right_bound,
                trusted_id,
            )
        }
    }
}
