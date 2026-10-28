fn main() {
    // The dashboard is embedded at compile time. Rebuild when `pnpm build` changes it.
    println!("cargo:rerun-if-changed=../../ui/dist");
}
