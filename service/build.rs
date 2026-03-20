fn main() {
    #[cfg(target_os = "windows")]
    embed_resource::compile("helper.rc", embed_resource::NONE);
}
