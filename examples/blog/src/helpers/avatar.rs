/// `{{ user.name | avatar_hue }}`: a hue (0-359) for someone's avatar. The same name always
/// gets the same color, on every page and across restarts (FNV-1a, not `std`'s random hasher).
#[kiln::template_filter]
pub fn avatar_hue(name: &str) -> u32 {
    let hash = name.bytes().fold(0x811c_9dc5_u32, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193));
    hash % 360
}
