use link::config::{self, LinkClientConfig};

#[test]
fn read_from_filepath() {
    let c = config::read_from_filepath::<LinkClientConfig>("tests/not_exist");
    assert!(c.is_err());

    let c = config::read_from_filepath::<LinkClientConfig>("tests/config.yaml");
    assert!(c.is_ok());
}
