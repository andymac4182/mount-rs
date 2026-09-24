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
