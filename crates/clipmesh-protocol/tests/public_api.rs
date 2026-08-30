use clipmesh_protocol::{FailureCode, FailureResponse, U64Decimal};

#[test]
fn valid_outbound_protocol_values_are_constructible_externally() {
    let failure = FailureResponse::new(FailureCode::PayloadEmpty);
    assert_eq!(failure.code(), FailureCode::PayloadEmpty);
    assert!(!failure.retryable());
    assert_eq!(U64Decimal::new(2).unwrap().get(), 2);
    assert!(U64Decimal::new(0).is_err());
}
