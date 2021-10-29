# Module proofs

[UNSTABLE](UNSTABLE.md) Module for proving queried data.


## Functions
[proof_block_data](#proof_block_data) – Proves that given block's data, which is queried from DApp server, can be trusted. This function checks block proofs and compares given data with the proven. If the given data differs from the proven, the exception will be thrown. The input param is a single block's JSON object, which was queried from DApp server using functions such as `net.query`, `net.query_collection` or `net.wait_for_collection`. If block's BOC is not provided in the JSON, it will be queried from DApp server (in this case it is required to provide at least `id` of block).

## Types
[ProofsErrorCode](#ProofsErrorCode)

[ParamsOfProofBlockData](#ParamsOfProofBlockData)


# Functions
## proof_block_data

Proves that given block's data, which is queried from DApp server, can be trusted. This function checks block proofs and compares given data with the proven. If the given data differs from the proven, the exception will be thrown. The input param is a single block's JSON object, which was queried from DApp server using functions such as `net.query`, `net.query_collection` or `net.wait_for_collection`. If block's BOC is not provided in the JSON, it will be queried from DApp server (in this case it is required to provide at least `id` of block).

If `cache_proofs` in config is set to `true` (default), downloaded proofs and master-chain BOCs
are saved into the persistent local storage (e.g. file system for native environments or
browser's IndexedDB for the web); otherwise all the data is cached only in memory in current
client's context and will be lost after destruction of the client.

Why Proofs are Needed

Proofs are needed to ensure that the data downloaded from a DApp server is real blockchain
data. Checking proofs can protect from the malicious DApp server which can potentially provide
fake data, or also from "Man in the Middle" attacks class.

What Proofs are

Simply, proof is a list of signatures of validators', which have signed this particular master-
block.

The very first validator set's public keys are included in the zero-state. Whe know a root hash
of the zero-state, because it is stored in the network configuration file, it is our authority
root. For proving zero-state it is enough to calculate and compare its root hash.

In each new validator cycle the validator set is changed. The new one is stored in a key-block,
which is signed by the validator set, which we already trust, the next validator set will be
stored to the new key-block and signed by the current validator set, and so on.

In order to prove any block in the master-chain we need to check, that it has been signed by
a trusted validator set. So we need to check all key-blocks' proofs, started from the zero-state
and until the block, which we want to prove. But it can take a lot of time and traffic to
download and prove all key-blocks on a client. For solving this, special trusted blocks are used
in TON-SDK.

The trusted block is the authority root, as well, as the zero-state. Each trusted block is the
`id` (e.g. `root_hash`) of the already proven key-block. There can be plenty of trusted
blocks, so there can be a lot of authority roots. The hashes of trusted blocks for MainNet
and DevNet are hardcoded in SDK in a separated binary file (trusted_key_blocks.bin) and can
be updated for each release.
In future SDK releases, one will also be able to provide their hashes of trusted blocks for other
networks, besides for MainNet and DevNet.
By using trusted key-blocks, in order to prove any block, we can prove chain of key-blocks to the
closest previous trusted key-block, not only to the zero-state.

But shard-blocks don't have proofs on DApp server. In this case, in order to prove any shard-
block data, we search for a corresponding master-block, which contains the root hash of this shard-block,
or some shard block which is linked to that block in shard-chain. After proving this master-
block, we traverse through each link and calculate and compare hashes with links, one-by-one.
After that we can ensure that this shard-block has also been proven.

```ts
type ParamsOfProofBlockData = {
    block: any
}

function proof_block_data(
    params: ParamsOfProofBlockData,
): Promise<void>;
```
### Parameters
- `block`: _any_ – Single block's data that needs proof as queried from DApp server, without modifications. Required fields are `id` and/or top-level `boc` (for block identification), others are optional.


# Types
## ProofsErrorCode
```ts
enum ProofsErrorCode {
    InvalidData = 901,
    ProofCheckFailed = 902,
    InternalError = 903,
    DataDiffersFromProven = 904
}
```
One of the following value:

- `InvalidData = 901`
- `ProofCheckFailed = 902`
- `InternalError = 903`
- `DataDiffersFromProven = 904`


## ParamsOfProofBlockData
```ts
type ParamsOfProofBlockData = {
    block: any
}
```
- `block`: _any_ – Single block's data that needs proof as queried from DApp server, without modifications. Required fields are `id` and/or top-level `boc` (for block identification), others are optional.


