use mount_rs_remote_protocol::{
    MAX_FRAME_BYTES, Message, Operation, OperationName, PROTOCOL_VERSION, Permission, read_frame,
    write_frame,
};

#[tokio::test]
async fn framed_request_round_trips() {
    let (mut writer, mut reader) = tokio::io::duplex(4096);
    let sent = Message::Request {
        request_id: 7,
        drive_id: "data".into(),
        operation: Operation {
            name: OperationName::Stat,
            body: serde_json::json!({"path":"/file"}),
        },
    };
    write_frame(&mut writer, &sent).await.unwrap();
    let received = read_frame(&mut reader).await.unwrap();
    assert_eq!(sent, received);
}

#[tokio::test]
async fn oversized_frame_is_rejected_before_allocation() {
    use tokio::io::AsyncWriteExt;
    let (mut writer, mut reader) = tokio::io::duplex(4096);
    writer
        .write_all(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())
        .await
        .unwrap();
    assert!(read_frame(&mut reader).await.is_err());
}

#[test]
fn version_and_mutation_classification_are_explicit() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(
        Operation {
            name: OperationName::Stat,
            body: serde_json::Value::Null
        }
        .required_permission(),
        Permission::Read
    );
    assert_eq!(
        Operation {
            name: OperationName::Write,
            body: serde_json::Value::Null
        }
        .required_permission(),
        Permission::Write
    );
    assert_eq!(
        Operation {
            name: OperationName::Open,
            body: serde_json::json!({"flags":"r"})
        }
        .required_permission(),
        Permission::Read
    );
    assert_eq!(
        Operation {
            name: OperationName::Open,
            body: serde_json::json!({"flags":"w"})
        }
        .required_permission(),
        Permission::Write
    );
    assert_eq!(
        Operation {
            name: OperationName::Syncfs,
            body: serde_json::Value::Null
        }
        .required_permission(),
        Permission::Write
    );
}

#[test]
fn read_only_open_variants_do_not_require_write_grants() {
    assert_eq!(
        Operation {
            name: OperationName::Open,
            body: serde_json::json!({"flags":"rs"})
        }
        .required_permission(),
        Permission::Read
    );
    let mut body = serde_json::json!({"Open":{"flags":{"read":true,"write":false,"create":false,"truncate":false,"append":false,"exclusive":false}}});
    assert_eq!(
        Operation {
            name: OperationName::GuardedMutation,
            body: body.clone()
        }
        .required_permission(),
        Permission::Read
    );
    body["Open"]["flags"]["create"] = serde_json::json!(true);
    assert_eq!(
        Operation {
            name: OperationName::GuardedMutation,
            body
        }
        .required_permission(),
        Permission::Write
    );
}

#[test]
fn structured_open_flags_fail_closed_for_missing_or_wrong_types() {
    for name in [OperationName::Open, OperationName::GuardedMutation] {
        for field in ["read", "write", "create", "truncate", "append", "exclusive"] {
            for value in [
                None,
                Some(serde_json::json!(null)),
                Some(serde_json::json!("false")),
                Some(serde_json::json!(0)),
                Some(serde_json::json!(field != "read")),
            ] {
                let mut flags = serde_json::json!({"read":true,"write":false,"create":false,"truncate":false,"append":false,"exclusive":false});
                if let Some(value) = value {
                    flags[field] = value;
                } else {
                    flags.as_object_mut().unwrap().remove(field);
                }
                let body = if name == OperationName::Open {
                    serde_json::json!({"flags":flags})
                } else {
                    serde_json::json!({"Open":{"flags":flags}})
                };
                assert_eq!(
                    Operation { name, body }.required_permission(),
                    Permission::Write
                );
            }
        }
    }
}
