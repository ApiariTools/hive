fn main() {
    let dist = std::path::Path::new("web/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).ok();
        std::fs::write(
            dist.join("index.html"),
            "<!DOCTYPE html><html><body><p>Frontend not built. Run: cd web &amp;&amp; npm install &amp;&amp; npm run build</p></body></html>",
        )
        .ok();
    }
    println!("cargo::rerun-if-changed=web/dist/");
}
