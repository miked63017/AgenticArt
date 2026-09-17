//! Reference plugin: inverts RGB (leaves alpha untouched). Demonstrates the
//! full plugin ABI surface with the smallest possible filter.

fn invert(pixels: &mut [u8], _width: u32, _height: u32) {
    for p in pixels.chunks_exact_mut(4) {
        p[0] = 255 - p[0];
        p[1] = 255 - p[1];
        p[2] = 255 - p[2];
    }
}

agenticart_plugin_sdk::export_filter!(invert);
