use link::config;

#[test]
fn get_from_filepath() {
    let mut c = config::get_from_filepath("tests/not_exist");
    assert!(c.is_err());

    c = config::get_from_filepath("tests/config.yaml");
    assert!(c.is_ok());
}
