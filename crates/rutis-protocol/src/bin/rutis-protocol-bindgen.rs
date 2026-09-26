use rutis_protocol::{codegen::generate, contract::AdmittedBundle};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: rutis-protocol-bindgen BUNDLE RUST_OUTPUT TS_OUTPUT".into());
    }
    let bindings = generate(&AdmittedBundle::parse(&std::fs::read(&args[0])?)?);
    std::fs::write(&args[1], bindings.rust)?;
    std::fs::write(&args[2], bindings.typescript)?;
    Ok(())
}
