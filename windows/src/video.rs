//! Hardware decode (Media Foundation, D3D11-backed) and presentation (D3D11 video processor -> flip-model swapchain).
//! Decoded frames never leave the GPU.

use std::mem::ManuallyDrop;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
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
            Ok(Gpu { device, vdev, vctx, swap, tearing, size: (desc.Width, desc.Height), vp: None })
        }
    }

    pub fn present(&mut self, pic: &Picture, size: (u32, u32)) -> Result<()> {
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
                let s = (size.0 as f64 / pic.w as f64).min(size.1 as f64 / pic.h as f64);
                let (dw, dh) = ((pic.w as f64 * s) as i32, (pic.h as f64 * s) as i32);
                let (dx, dy) = ((size.0 as i32 - dw) / 2, (size.1 as i32 - dh) / 2);
                c.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&RECT { left: dx, top: dy, right: dx + dw, bottom: dy + dh }));
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
            let flags = if self.tearing { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) };
            self.swap.Present(0, flags).ok()
        }
    }
}
