//! Software preview of a compiled `.bal2d` pack: what the watch will show, rendered on the host
//! or in the AstroBox plugin (wasm) without a GPU.

pub use bal2d_compiler::raster;

use bal2d_core::{Lobby, LoadError, Touch};
use bal2d_render::DrawList;

pub use raster::{Camera, Framebuffer, Texture};

/// A loaded pack with its pages decoded for the rasterizer.
pub struct PackPreview {
    pub lobby: Lobby,
    pages: Vec<Vec<Texture>>,
    list: DrawList,
    /// The baked bottom of the scene as (x, y, w, h, linear RGB) for the framebuffer.
    bake: Option<(usize, usize, usize, usize, Vec<[f32; 4]>)>,
    /// Draw like the watch's GPU with this clustering tolerance (`raster::draw_as_gpu`); `None`:
    /// exact texture mapping.
    pub gpu_tolerance: Option<f32>,
}

/// Decodes a BAKE section into framebuffer pixels.
pub fn decode_bake(pack: &[u8], bake: &bal2d_core::Bake) -> Result<(usize, usize, usize, usize, Vec<[f32; 4]>), bal2d_pack::PackError> {
    let mut section = vec![0u8; bake.section_len()];
    bal2d_pack::read_section(&mut &pack[..], &bake.entry, &mut section)?;
    let h = bake.header;
    let px = section[bal2d_core::Bake::PREFIX..]
        .chunks_exact(2)
        .map(|b| {
            let v = u16::from_le_bytes([b[0], b[1]]);
            [((v >> 11) & 31) as f32 / 31.0, ((v >> 5) & 63) as f32 / 63.0, (v & 31) as f32 / 31.0, 1.0]
        })
        .collect();
    Ok((h.x as usize, h.y as usize, h.width as usize, h.height as usize, px))
}

/// Copies a decoded bake into the framebuffer (it is opaque and drawn first).
pub fn draw_bake(fb: &mut Framebuffer, bake: &(usize, usize, usize, usize, Vec<[f32; 4]>)) {
    let (x0, y0, w, h, px) = bake;
    for y in 0..*h {
        let fy = y0 + y;
        if fy >= fb.height {
            break;
        }
        for x in 0..*w {
            let fx = x0 + x;
            if fx < fb.width {
                fb.pixels[fy * fb.width + fx] = px[y * w + x];
            }
        }
    }
}

impl PackPreview {
    pub fn load(pack: &[u8]) -> Result<PackPreview, LoadError> {
        let (mut lobby, pages) = Lobby::load(pack)?;
        let mut per_layer: Vec<Vec<Texture>> = (0..lobby.layers.len()).map(|_| Vec::new()).collect();
        for p in &pages {
            let mut section = vec![0u8; p.section_len()];
            bal2d_core::read_page(&mut &pack[..], p, &mut section).map_err(LoadError::Pack)?;
            let img = bal2d_compiler::image::decode_page(p.header.width as u32, p.header.height as u32, p.header.format, &section[8..]);
            per_layer[p.layer as usize].push(Texture::from_premultiplied_rgba8(img.width as usize, img.height as usize, &img.data));
        }
        let bake = match &lobby.bake {
            Some(b) => Some(decode_bake(pack, b).map_err(LoadError::Pack)?),
            None => None,
        };
        lobby.start();
        let gpu_tolerance = Some(bal2d_render::Paths::new().tolerance);
        Ok(PackPreview { lobby, pages: per_layer, list: DrawList::default(), bake, gpu_tolerance })
    }

    /// Layer `layer`'s decoded pages.
    pub fn pages(&self, layer: usize) -> &[Texture] {
        &self.pages[layer]
    }

    pub fn size(&self) -> (usize, usize) {
        (self.lobby.width as usize, self.lobby.height as usize)
    }

    /// Advances the lobby by `seconds` in 30 Hz steps.
    pub fn advance(&mut self, seconds: f32) {
        let mut t = 0.0;
        while t < seconds {
            self.lobby.update(1.0 / 30.0);
            self.lobby.notices.clear();
            t += 1.0 / 30.0;
        }
    }

    pub fn touch(&mut self, kind: Touch, x: f32, y: f32) {
        self.lobby.touch(kind, x, y);
    }

    /// The current pose, opaque RGBA8 over black.
    pub fn render(&mut self) -> Vec<u8> {
        self.render_layers(u32::MAX)
    }

    /// `render` with only the layers in `mask` (bit = layer index).
    pub fn render_layers(&mut self, mask: u32) -> Vec<u8> {
        let (w, h) = self.size();
        let mut fb = Framebuffer::new(w, h, [0.0, 0.0, 0.0, 1.0]);
        if let Some(b) = self.bake.as_ref().filter(|_| self.lobby.bake_active()) {
            draw_bake(&mut fb, b);
        }
        let c = self.lobby.camera;
        let cam = Camera { cx: c.cx, cy: c.cy, scale: c.scale };
        for li in 0..self.lobby.layers.len() {
            if mask & (1 << li) == 0 {
                continue;
            }
            self.lobby.draw_list(li, &mut self.list);
            raster::draw_as_gpu(&mut fb, &self.list, &self.pages[li], cam, self.gpu_tolerance);
        }
        fb.to_rgba8()
    }
}
