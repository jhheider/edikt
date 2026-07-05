fn main() {
    // Scaffold only. The CLI (clap, stdin/stdout, -i/-e/-f, format dispatch,
    // the output contract) arrives with the M1 skeleton slice.
    println!("edikt {}", env!("CARGO_PKG_VERSION"));
}
