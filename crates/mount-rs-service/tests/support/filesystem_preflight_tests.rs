use super::*;

#[test]
fn filesystem_owner_marker_rejects_foreign_noncanonical_and_unknown_fields() {
    let owner = "mount-rs-filesystem-0123456789abcdef01234567";
    let identity = Identity {
        dev: "17".into(),
        ino: "49".into(),
        uid: "501".into(),
    };
    let valid = json!({"schema":"mount-rs-filesystem-owner-v1","owner":owner,
        "root":{"dev":"17","ino":"49","uid":"501"}});
    let parsed: OwnerMarker = serde_json::from_value(valid.clone()).unwrap();
    validate_marker(&parsed, owner, &identity).unwrap();
    for (pointer, changed) in [
        ("/schema", json!("foreign")),
        (
            "/owner",
            json!("mount-rs-filesystem-ffffffffffffffffffffffff"),
        ),
        ("/root/dev", json!("18")),
        ("/root/ino", json!("50")),
        ("/root/uid", json!("502")),
        ("/root/dev", json!("017")),
        ("/root/ino", json!("+49")),
        ("/root/uid", json!("501\n")),
        ("/root/ino", json!("18446744073709551616")),
    ] {
        let mut changed_value = valid.clone();
        *changed_value.pointer_mut(pointer).unwrap() = changed;
        let parsed: OwnerMarker = serde_json::from_value(changed_value).unwrap();
        assert!(validate_marker(&parsed, owner, &identity).is_err());
    }
    for invalid in [
        "",
        "mount-rs-filesystem-short",
        "mount-rs-filesystem-0123456789ABCDEF01234567",
        "foreign",
    ] {
        assert!(!valid_owner(invalid));
    }
    let mut extra = valid;
    extra["unrecognized"] = json!(true);
    assert!(serde_json::from_value::<OwnerMarker>(extra).is_err());
    for value in ["0", "17", "18446744073709551615"] {
        number(value).unwrap();
    }
}
