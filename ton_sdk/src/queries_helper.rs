use crate::*;
use graphite::client::GqlClient;
use graphite::types::VariableRequest;
use futures::stream::Stream;
use serde_json::Value;
use std::sync::Mutex;

lazy_static! {
    static ref CLIENT: Mutex<Option<GqlClient>> = Mutex::new(None);
}

// Globally initializes client with server address
pub fn init(config: QueriesConfig) {
    let mut client = CLIENT.lock().unwrap();
    *client = Some(GqlClient::new(&config.queries_server,&config.subscriptions_server));
}

pub fn uninit() {
    let mut client = CLIENT.lock().unwrap();
    *client = None;
}

// Returns Stream with updates of some field in database. First stream item is current value
pub fn subscribe_record_updates(table: &'static str, record_id: &str, fields: &str)
    -> SdkResult<Box<dyn Stream<Item=Value, Error=SdkError>>> {

    let load_stream = load_record_fields(table, record_id, fields)?
        .filter(|value| !value.is_null());

    let request = generate_subscription(table, record_id, fields);

    if let Some(client) = CLIENT.lock().unwrap().as_mut() {
        let stream = client.subscribe(request)?
            .then(move |result| {
                match result {
                    Err(err) => Err(SdkError::from(err)),
                    Ok(value) => {
                        // try to extract the record value from the answer
                        let record_value = &value["payload"]["data"][table];
                        
                        if record_value.is_null() {
                            Err(SdkError::from(SdkErrorKind::InvalidData(
                                format!("Invalid subscription answer: {}", value))))
                        } else {
                            Ok(record_value.clone())
                        }
                    }
                }
            });

        Ok(Box::new(load_stream.chain(stream)))
    } else {
        bail!(SdkErrorKind::NotInitialized)
    }
}

fn rename_key_to_id(value: serde_json::Value) -> SdkResult<serde_json::Value> {
    if let serde_json::Value::Object(mut obj) = value {
        let id = obj.get("_key").map(|v| v.clone());
        if let Some(id) = id {
            obj.insert("id".to_string(), id);
            obj.remove("_key");
            Ok(serde_json::Value::Object(obj))
        } else {
            bail!(SdkErrorKind::InvalidData("rename_key_to_id: id not found".into()))
        }
    } else {
        bail!(SdkErrorKind::InvalidData("rename_key_to_id: invalid json value".into()))
    }
}

// Returns Stream with required database record
pub fn load_record(table: &str, record_id: &str)
    -> SdkResult<Box<dyn Stream<Item=Value, Error=SdkError>>> {

    let query = generate_select(table, record_id);

    let mut client = CLIENT.lock().unwrap();
    let client = client.as_mut().ok_or(SdkError::from(SdkErrorKind::NotInitialized))?;

    let stream = client.query_vars(query)?
        .then(|result| {
            match result {
                Err(err) => Err(SdkError::from(err)),
                Ok(value) => {
                    // try to extract the record value from the answer
                    let records_array_str = value["data"]["select"].as_str()
                            .ok_or(SdkError::from(SdkErrorKind::InvalidData(
                                format!("Invalid select answer: {}", value))))?;

                    let records_array: serde_json::Value = serde_json::from_str(records_array_str)?;

                    let record_value = &records_array[0];

                    // `null` is Ok - it means that query execution was succeded but no record found
                    if record_value.is_null() {
                        Ok(record_value.clone())
                    } else {
                        Ok(rename_key_to_id(record_value.clone())?)
                    }
                }
            }
        });

    Ok(Box::new(stream))
}

// Returns Stream with required database record fields
pub fn load_record_fields(table: &'static str, record_id: &str, fields: &str)
    -> SdkResult<Box<dyn Stream<Item=Value, Error=SdkError>>> {

    let query = generate_query(table, record_id, fields);

    let mut client = CLIENT.lock().unwrap();
    let client = client.as_mut().ok_or(SdkError::from(SdkErrorKind::NotInitialized))?;

    let stream = client.query(query)?
        .then(move |result| {
            match result {
                Err(err) => Err(SdkError::from(err)),
                Ok(value) => {
                    // try to extract the record value from the answer
                    let records_array = &value["data"][table];
                    if records_array.is_null() {
                        bail!(SdkErrorKind::InvalidData(format!("Invalid select answer: {}", value)))
                    }
                    
                    Ok(records_array[0].clone())
                }
            }
        });

    Ok(Box::new(stream))
}

fn generate_query(table: &str, record_id: &str, fields: &str) -> String {
    format!("query {table} {{ {table}(filter: \"{{ \\\"match\\\": {{ \\\"id\\\": \\\"{record_id}\\\" }} }}\") {{ {fields} }} }}",
        table=table,
        record_id=record_id,
        fields=fields)
}

fn generate_select(table: &str, record_id: &str) -> VariableRequest {
    let query = "query select($query: String!, $bindVarsJson: String!) {select(query: $query, bindVarsJson: $bindVarsJson)}".to_owned();

    let db_query = format!("RETURN DOCUMENT(\"{table}/{record_id}\")", table=table, record_id=record_id);

    let variables = json!({"query" : db_query,"bindVarsJson": "{}"});

    VariableRequest::new(query, Some(variables.to_string()))
}

fn generate_subscription(table: &str, record_id: &str, fields: &str) -> VariableRequest {
    let query = format!("subscription {table}($match: String) {{ {table}(match: $match) {{ {fields} }} }}",
        table=table,
        fields=fields);

    let variables = format!("{{\"match\":\"{{\\\"id\\\":\\\"{record_id}\\\"}}\"}}",
        record_id=record_id);

    VariableRequest::new(query, Some(variables))
}
