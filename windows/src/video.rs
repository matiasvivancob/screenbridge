//! Hardware decode (Media Foundation, D3D11-backed) and presentation (D3D11 video processor -> flip-model swapchain).
//! Decoded frames never leave the GPU.

use crate::cursor::CursorImage;
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::*;

/// A decoded picture: the decoder's sample keeps the texture alive.
pub struct Picture {
    _sample: IMFSample,
    tex: ID3D11Texture2D,
    sub: u32,
    pub w: u32,
    pub h: u32,
}

pub struct Decoder {
    mft: IMFTransform,
    _mgr: IMFDXGIDeviceManager,
    /// Codec id as in the packet header: 0 H.264, 1 HEVC, 2 AV1, 3 VP9.
    pub codec: u8,
    size: (u32, u32),
    t: i64,
}

impl Decoder {
    pub fn new(device: &ID3D11Device, codec: u8) -> Result<Self> {
        unsafe {
            let (sub, needs) = match codec {
                1 => (MFVideoFormat_HEVC, "HEVC needs Microsoft's HEVC Video Extensions"),
                2 => (MFVideoFormat_AV1, "AV1 needs Microsoft's AV1 Video Extension"),
                3 => (MFVideoFormat_VP90, "VP9 needs Microsoft's VP9 Video Extensions"),
                _ => (MFVideoFormat_H264, "no H.264 decoder"),
            };
            let info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: sub };
            let mut acts: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut n = 0u32;
            // Sync MFTs: Microsoft's H.264/HEVC decoders, which use DXVA once given a D3D manager.
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&info),
                None,
                &mut acts,
                &mut n,
            )?;
            if n == 0 {
                return Err(Error::new(E_FAIL, needs));
            }
            let list = std::slice::from_raw_parts_mut(acts, n as usize);
            let mft: IMFTransform = list[0].as_ref().unwrap().ActivateObject()?;
            for a in list.iter_mut() {
                a.take();
            }
            CoTaskMemFree(Some(acts as *const _));

            mft.GetAttributes()?.SetUINT32(&MF_LOW_LATENCY, 1)?;
            let mut token = 0;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
            let mgr = mgr.unwrap();
            mgr.ResetDevice(device, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize)?;

            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, &sub)?;
            // A provisional size; the decoder signals a stream change with the real one from the SPS.
            t.SetUINT64(&MF_MT_FRAME_SIZE, (1920u64 << 32) | 1080)?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &t, 0)?;
            let mut d = Decoder { mft, _mgr: mgr, codec, size: (0, 0), t: 0 };
            let _ = d.set_output(); // some decoders only offer output types after the first SPS
            d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(d)
        }
    }

    fn set_output(&mut self) -> Result<()> {
        unsafe {
            for i in 0.. {
                let t = self.mft.GetOutputAvailableType(0, i)?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    let fs = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    self.size = ((fs >> 32) as u32, fs as u32);
                    let flags = self.mft.GetOutputStreamInfo(0)?.dwFlags;
                    if flags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 == 0 {
                        return Err(Error::new(E_FAIL, "decoder does not output GPU textures"));
                    }
                    return Ok(());
                }
            }
            unreachable!()
        }
    }

    /// One complete access unit in (Annex-B), newest decoded picture out.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<Picture>> {
        unsafe {
            let buf = MFCreateMemoryBuffer(au.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), p, au.len());
            buf.Unlock()?;
            buf.SetCurrentLength(au.len() as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(self.t)?;
            s.SetSampleDuration(166_667)?;
            self.t += 166_667;
            self.mft.ProcessInput(0, &s, 0)?;

            let mut latest = None;
            loop {
                let mut out = [MFT_OUTPUT_DATA_BUFFER::default()];
                let mut status = 0;
                let r = self.mft.ProcessOutput(0, &mut out, &mut status);
                let sample = ManuallyDrop::into_inner(std::mem::take(&mut out[0].pSample));
                drop(ManuallyDrop::into_inner(std::mem::take(&mut out[0].pEvents)));
                match r {
                    Ok(()) => {
                        if let Some(sample) = sample {
                            let dx: IMFDXGIBuffer = sample.GetBufferByIndex(0)?.cast()?;
                            let mut tex: *mut core::ffi::c_void = std::ptr::null_mut();
                            dx.GetResource(&ID3D11Texture2D::IID, &mut tex)?;
                            let tex = ID3D11Texture2D::from_raw(tex);
                            let sub = dx.GetSubresourceIndex()?;
                            latest = Some(Picture { _sample: sample, tex, sub, w: self.size.0, h: self.size.1 });
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.set_output()?,
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(latest),
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

pub struct Gpu {
    pub device: ID3D11Device,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext1,
    swap: IDXGISwapChain1,
    tearing: bool,
    size: (u32, u32),
    vp: Option<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor, [u32; 4])>,
    cursor_code: (ID3DBlob, ID3DBlob),
    cursor_draw: Option<CursorDraw>,
}

pub struct Cursor<'a> {
    pub image: &'a Arc<CursorImage>,
    pub shape: u16,
    pub x: i16,
    pub y: i16,
}

const CURSOR_SHADER: &str = r#"
cbuffer Rect : register(b0) { float4 rect; };
struct Vertex { float4 position : SV_Position; float2 uv : TEXCOORD0; };
Vertex vs(uint id : SV_VertexID) {
    Vertex v;
    v.uv = float2(id & 1, id >> 1);
    v.position = float4(rect.xy + v.uv * rect.zw, 0, 1);
    return v;
}
Texture2D sprite : register(t0);
SamplerState pointSampler : register(s0);
float4 ps(Vertex v) : SV_Target { return sprite.Sample(pointSampler, v.uv); }
"#;

fn cursor_shader(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    unsafe {
        let mut code = None;
        D3DCompile(CURSOR_SHADER.as_ptr() as _, CURSOR_SHADER.len(), None, None, None, entry, target, 0, 0, &mut code, None)?;
        code.ok_or_else(|| Error::new(E_FAIL, "no cursor shader bytecode"))
    }
}

struct CursorDraw {
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    rect: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    depth: ID3D11DepthStencilState,
    textures: VecDeque<(u16, Arc<CursorImage>, ID3D11ShaderResourceView)>,
}

impl CursorDraw {
    fn new(device: &ID3D11Device, code: &(ID3DBlob, ID3DBlob)) -> Result<Self> {
        unsafe {
            let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize());
            let (mut vs, mut ps) = (None, None);
            device.CreateVertexShader(bytes(&code.0), None, Some(&mut vs))?;
            device.CreatePixelShader(bytes(&code.1), None, Some(&mut ps))?;
            let mut rect = None;
            let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            device.CreateBuffer(&desc, None, Some(&mut rect))?;
            let mut sampler = None;
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&desc, Some(&mut sampler))?;
            let mut blend = None;
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            device.CreateBlendState(&desc, Some(&mut blend))?;
            let mut raster = None;
            let desc = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() };
            device.CreateRasterizerState(&desc, Some(&mut raster))?;
            let mut depth = None;
            let face = D3D11_DEPTH_STENCILOP_DESC { StencilFailOp: D3D11_STENCIL_OP_KEEP, StencilDepthFailOp: D3D11_STENCIL_OP_KEEP, StencilPassOp: D3D11_STENCIL_OP_KEEP, StencilFunc: D3D11_COMPARISON_ALWAYS };
            let desc = D3D11_DEPTH_STENCIL_DESC { DepthFunc: D3D11_COMPARISON_ALWAYS, DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ZERO, FrontFace: face, BackFace: face, ..Default::default() };
            device.CreateDepthStencilState(&desc, Some(&mut depth))?;
            Ok(Self { ctx: device.GetImmediateContext()?, vs: vs.unwrap(), ps: ps.unwrap(), rect: rect.unwrap(), sampler: sampler.unwrap(), blend: blend.unwrap(), raster: raster.unwrap(), depth: depth.unwrap(), textures: VecDeque::new() })
        }
    }

    fn draw(&mut self, device: &ID3D11Device, back: &ID3D11Texture2D, pic: &Picture, size: (u32, u32), cursor: Cursor<'_>) -> Result<()> {
        unsafe {
            let image = cursor.image;
            // IDs can be reused after a reconnect; also compare the stored image before reusing its texture.
            let texture = if let Some(texture) = self.textures.iter().position(|(id, old, _)| *id == cursor.shape && Arc::ptr_eq(old, image)).and_then(|i| self.textures.remove(i)) {
                texture
            } else {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: image.w,
                    Height: image.h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                };
                let data = D3D11_SUBRESOURCE_DATA { pSysMem: image.bgra.as_ptr() as _, SysMemPitch: image.w * 4, ..Default::default() };
                let mut tex = None;
                device.CreateTexture2D(&desc, Some(&data), Some(&mut tex))?;
                let mut view = None;
                device.CreateShaderResourceView(&tex.unwrap(), None, Some(&mut view))?;
                self.textures.retain(|(id, _, _)| *id != cursor.shape);
                (cursor.shape, image.clone(), view.unwrap())
            };
            let view = texture.2.clone();
            self.textures.push_back(texture);
            if self.textures.len() > 8 {
                self.textures.pop_front();
            }

            let dest = letterbox((pic.w, pic.h), size);
            let s = (dest.right - dest.left) as f32 / pic.w as f32;
            let x = dest.left as f32 + (cursor.x as f32 - image.hx as f32) * s;
            let y = dest.top as f32 + (cursor.y as f32 - image.hy as f32) * s;
            let rect = [x * 2. / size.0 as f32 - 1., 1. - y * 2. / size.1 as f32, image.w as f32 * s * 2. / size.0 as f32, -(image.h as f32) * s * 2. / size.1 as f32];
            let mut target = None;
            device.CreateRenderTargetView(back, None, Some(&mut target))?;
            let c = &self.ctx;
            c.UpdateSubresource(&self.rect, 0, None, rect.as_ptr() as _, 0, 0);
            c.IASetInputLayout(None);
            c.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            c.VSSetShader(&self.vs, None);
            c.VSSetConstantBuffers(0, Some(&[Some(self.rect.clone())]));
            c.PSSetShader(&self.ps, None);
            c.PSSetShaderResources(0, Some(&[Some(view)]));
            c.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            c.RSSetState(&self.raster);
            c.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: size.0 as f32, Height: size.1 as f32, MinDepth: 0., MaxDepth: 1., ..Default::default() }]));
            c.OMSetBlendState(&self.blend, None, u32::MAX);
            c.OMSetDepthStencilState(&self.depth, 0);
            c.OMSetRenderTargets(Some(&[target]), None);
            c.Draw(4, 0);
            // The context must not retain a back-buffer reference across ResizeBuffers.
            c.OMSetRenderTargets(None, None);
            c.PSSetShaderResources(0, Some(&[None]));
            Ok(())
        }
    }
}

fn letterbox(picture: (u32, u32), size: (u32, u32)) -> RECT {
    let s = (size.0 as f64 / picture.0 as f64).min(size.1 as f64 / picture.1 as f64);
    let (dw, dh) = ((picture.0 as f64 * s) as i32, (picture.1 as f64 * s) as i32);
    let (dx, dy) = ((size.0 as i32 - dw) / 2, (size.1 as i32 - dh) / 2);
    RECT { left: dx, top: dy, right: dx + dw, bottom: dy + dh }
}

impl Gpu {
    /// Uses the GPU that drives `monitor`, so frames don't cross adapters.
    pub fn new(hwnd: HWND, monitor: HMONITOR, size: (u32, u32)) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
            let mut adapter: Option<IDXGIAdapter> = None;
            'outer: for a in 0.. {
                let Ok(ad) = factory.EnumAdapters1(a) else { break };
                for o in 0.. {
                    let Ok(out) = ad.EnumOutputs(o) else { break };
                    if out.GetDesc()?.Monitor == monitor {
                        adapter = Some(ad.cast()?);
                        break 'outer;
                    }
                }
            }
            let mut device = None;
            D3D11CreateDevice(
                adapter.as_ref(),
                if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
            let device: ID3D11Device = device.unwrap();
            _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true); // the decoder shares this device
            device.cast::<IDXGIDevice1>()?.SetMaximumFrameLatency(1)?;
            let vdev: ID3D11VideoDevice = device.cast()?;
            let vctx: ID3D11VideoContext1 = device.GetImmediateContext()?.cast()?;

            let mut allow = BOOL(0);
            let tearing = factory
                .cast::<IDXGIFactory5>()
                .and_then(|f| f.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut allow as *mut _ as _, 4))
                .is_ok()
                && allow.as_bool();
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0.max(1),
                Height: size.1.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: if tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
                Stereo: false.into(),
            };
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;
            let cursor_code = (cursor_shader(s!("vs"), s!("vs_4_0"))?, cursor_shader(s!("ps"), s!("ps_4_0"))?);
            Ok(Gpu { device, vdev, vctx, swap, tearing, size: (desc.Width, desc.Height), vp: None, cursor_code, cursor_draw: None })
        }
    }

    pub fn present(&mut self, pic: &Picture, size: (u32, u32), cursor: Option<Cursor<'_>>) -> Result<()> {
        unsafe {
            let size = (size.0.max(1), size.1.max(1));
            if size != self.size {
                let flags = if self.tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING } else { DXGI_SWAP_CHAIN_FLAG(0) };
                self.swap.ResizeBuffers(0, size.0, size.1, DXGI_FORMAT_UNKNOWN, flags)?;
                self.size = size;
            }
            let key = [pic.w, pic.h, size.0, size.1];
            if self.vp.as_ref().is_none_or(|v| v.2 != key) {
                let cd = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                    InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    InputWidth: pic.w,
                    InputHeight: pic.h,
                    OutputWidth: size.0,
                    OutputHeight: size.1,
                    Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                    ..Default::default()
                };
                let en = self.vdev.CreateVideoProcessorEnumerator(&cd)?;
                let vp = self.vdev.CreateVideoProcessor(&en, 0)?;
                let c = &self.vctx;
                c.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
                c.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
                c.VideoProcessorSetStreamColorSpace1(&vp, 0, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
                c.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                let src = RECT { left: 0, top: 0, right: pic.w as i32, bottom: pic.h as i32 };
                c.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&src));
                // Letterbox: keep the stream's aspect ratio inside the window.
                c.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&letterbox((pic.w, pic.h), size)));
                let black = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0., G: 0., B: 0., A: 1. } } };
                c.VideoProcessorSetOutputBackgroundColor(&vp, false, &black);
                self.vp = Some((en, vp, key));
            }
            let (en, vp, _) = self.vp.as_ref().unwrap();

            let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: pic.sub } },
            };
            let mut iv = None;
            self.vdev.CreateVideoProcessorInputView(&pic.tex, en, &ivd, Some(&mut iv))?;
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let ovd = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut ov = None;
            self.vdev.CreateVideoProcessorOutputView(&back, en, &ovd, Some(&mut ov))?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: ManuallyDrop::new(iv), ..Default::default() };
            let r = self.vctx.VideoProcessorBlt(vp, ov.as_ref(), 0, std::slice::from_ref(&stream));
            drop(ManuallyDrop::take(&mut stream.pInputSurface));
            r?;
            if let Some(cursor) = cursor {
                if self.cursor_draw.is_none() {
                    self.cursor_draw = Some(CursorDraw::new(&self.device, &self.cursor_code)?);
                }
                if let Some(draw) = &mut self.cursor_draw {
                    draw.draw(&self.device, &back, pic, size, cursor)?;
                }
            }
            let flags = if self.tearing { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) };
            self.swap.Present(0, flags).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_shaders_compile_without_a_display() {
        assert!(cursor_shader(s!("vs"), s!("vs_4_0")).is_ok());
        assert!(cursor_shader(s!("ps"), s!("ps_4_0")).is_ok());
    }

    #[test]
    fn letterbox_destinations() {
        let wide = letterbox((1920, 1080), (2560, 1080));
        assert_eq!((wide.left, wide.top, wide.right, wide.bottom), (320, 0, 2240, 1080));
        let tall = letterbox((1920, 1080), (1920, 1440));
        assert_eq!((tall.left, tall.top, tall.right, tall.bottom), (0, 180, 1920, 1260));
        let scaled = letterbox((1920, 1080), (960, 540));
        assert_eq!((scaled.left, scaled.top, scaled.right, scaled.bottom), (0, 0, 960, 540));
    }
}
