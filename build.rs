//! Embeds the icon and version information into gromak.exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/gromak.rc");
    println!("cargo:rerun-if-changed=assets/gromak.ico");
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    let macros = [format!("GROMAK_VERSION_NUM={major},{minor},{patch},0"), format!("GROMAK_VERSION_STR=\"{version}\"")];
    embed_resource::compile("assets/gromak.rc", &macros).manifest_optional().expect("could not embed resources");
}
