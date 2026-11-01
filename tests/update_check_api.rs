#[test]
fn startup_requires_the_captured_environment() {
    let _: fn(&std::path::Path, u64, &namedrop::environment::EnvironmentSnapshot) =
        namedrop::update_check::start_with_environment;
}
