fn main() {
    // The fluent style follows the system light/dark preference on all platforms.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/main.slint", config).expect("Slint UI failed to compile");
}
