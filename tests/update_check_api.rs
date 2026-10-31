#[test]
fn start_preserves_the_two_argument_library_entrypoint() {
    let _: fn(&std::path::Path, u64) = namedrop::update_check::start;
}
