use clipmesh_protocol::{CredentialGeneration, FailureCode, FailureResponse};

#[test]
fn valid_outbound_control_values_are_constructible_externally() {
    let failure = FailureResponse::non_secret(FailureCode::PayloadEmpty).unwrap();
    assert_eq!(failure.code(), &FailureCode::PayloadEmpty);
    assert!(FailureResponse::non_secret(FailureCode::SecretResultAlreadyCommitted).is_err());
    assert_eq!(CredentialGeneration::new(2).unwrap().get(), 2);
    assert!(CredentialGeneration::new(0).is_err());
}
