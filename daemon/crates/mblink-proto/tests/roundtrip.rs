use mblink_proto::mblink::link::v1::{ConfigEntry, SetConfigRequest};
use prost::Message;

/// Codegen smoke test: proves tonic/prost generated the named types
/// (`SetConfigRequest`, `ConfigEntry`) with the named fields from
/// `proto/mblink/link/v1/link.proto`, and that they support Message encode/decode
/// (clone + PartialEq derive) round-trip.
#[test]
fn set_config_request_roundtrips() {
    let entry = ConfigEntry {
        package: "com.google.android.gm".into(),
        enabled: false,
        updated_at: 1,
        updated_by: "linux".into(),
    };
    let req = SetConfigRequest {
        module: "gm".into(),
        updates: vec![entry.clone()],
    };

    let mut bytes = Vec::new();
    req.encode(&mut bytes).unwrap();
    let decoded = SetConfigRequest::decode(bytes.as_slice()).unwrap();

    assert_eq!(decoded.module, "gm");
    assert_eq!(decoded.updates.len(), 1);
    assert_eq!(decoded.updates[0], entry);
}
