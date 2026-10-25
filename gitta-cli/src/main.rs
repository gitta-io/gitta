// mimalloc as the global allocator: ~2.6x faster than musl's default allocator on gitta's
// parse + cross-agent analysis hot path (measured on the shipped x86_64-musl target).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() { gitta::main_with(&gitta::Ext::default()) }
