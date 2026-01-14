#![no_std]
#![no_main]

extern crate alloc;

mod fft;
mod mp3;
mod utils;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use core::{ffi::c_void, ptr};
use fft::Analyzer;
use mp3::Mp3Player;
use psp::sys;
use psp::sys::ClearBuffer;
use psp::sys::GuContextType;
use psp::sys::GuPrimitive;
use psp::sys::GuState;
use psp::sys::GuSyncBehavior;
use psp::sys::GuSyncMode;
use psp::sys::VertexType;
use psp::vram_alloc::get_vram_allocator;
use psp::{Align16, BUF_WIDTH, SCREEN_HEIGHT, SCREEN_WIDTH};

// static GU list buffer
static mut LIST: Align16<[u32; 0x40000]> = Align16([0; 0x40000]);

// pointer to 1x1 white texture stored in VRAM (set in init_gu)
static mut WHITE_VRAM_PTR: *mut core::ffi::c_void = core::ptr::null_mut();

#[repr(C, align(4))]
struct ColVertex {
    color: u32,
    x: f32,
    y: f32,
    z: f32,
}

// persistent CPU-side vertex buffer to avoid calling `sceGuGetMemory` each frame
static mut VERTEX_BUFFER: Align16<[u8; 16 * (64 * 2)]> = Align16([0; 16 * (64 * 2)]);

const SPECTRUM_SIZE: usize = 64;
static mut SPECTRUM: Align16<[f32; SPECTRUM_SIZE]> = Align16([0.0; SPECTRUM_SIZE]);
static SPECTRUM_GEN: AtomicI32 = AtomicI32::new(0);
static SPECTRUM_STOP: AtomicBool = AtomicBool::new(false);

psp::module!("Musializer PSP", 1, 0);

fn psp_main() {
    unsafe { sys::sceKernelSetCompiledSdkVersion(0x06060010) };

    psp::enable_home_button();

    unsafe { init_gu() };

    psp::dprintln!("musializer-psp: starting MP3 player integration test");

    let path = "ms0:/PSP/GAME/Project/assets/sounds/mp3/compressed/ost_01_stripped_5s.mp3";

    match Mp3Player::open(path) {
        Ok(mut player) => {
            psp::dprintln!("MP3 player started");
            // Create Analyzer on heap and start FFT worker thread.
            let analyzer = Box::new(Analyzer::new());
            let analyzer_ptr = Box::into_raw(analyzer);

            let shared_ptr = player.raw_shared_ptr();
            let fft_args = Box::new(FftArgs {
                shared_ptr,
                analyzer: analyzer_ptr,
            });
            let fft_args_ptr = Box::into_raw(fft_args);

            let fft_thid = unsafe {
                sys::sceKernelCreateThread(
                    b"fft_thread\0".as_ptr(),
                    fft_thread_main,
                    0x2F,   // lower priority than audio thread (audio uses 0x1F)
                    0x4000, // 16KB stack
                    sys::ThreadAttributes::USER,
                    ptr::null_mut(),
                )
            };

            if fft_thid.0 >= 0 {
                let _ = unsafe {
                    sys::sceKernelStartThread(
                        fft_thid,
                        core::mem::size_of::<*mut FftArgs>(),
                        &fft_args_ptr as *const _ as *mut c_void,
                    )
                };
            }

            // precompute xs
            let margin = 20.0f32;
            let width_avail = SCREEN_WIDTH as f32 - margin * 2.0f32;
            let cell_w = width_avail / (SPECTRUM_SIZE as f32);
            let precomputed_xs: [f32; SPECTRUM_SIZE] = {
                let mut xs = [0.0f32; SPECTRUM_SIZE];
                for i in 0..SPECTRUM_SIZE {
                    xs[i] = margin + i as f32 * cell_w;
                }
                xs
            };

            // local render loop reads SPECTRUM written by FFT thread
            loop {
                match player.tick() {
                    Ok(true) => {
                        // copy shared spectrum snapshot into local fixed-size buffer
                        let display_m = SPECTRUM_SIZE;
                        let mut local = [0.0f32; SPECTRUM_SIZE];
                        let _gen = SPECTRUM_GEN.load(Ordering::Acquire);
                        unsafe {
                            for i in 0..display_m {
                                local[i] = SPECTRUM.0[i];
                            }
                        }

                        // draw frame with GU
                        unsafe {
                            sys::sceGuStart(
                                GuContextType::Direct,
                                &raw mut LIST.0 as *mut _ as *mut c_void,
                            );
                            sys::sceGuClearColor(0xFF000000);
                            sys::sceGuClearDepth(0);
                            sys::sceGuClear(
                                ClearBuffer::COLOR_BUFFER_BIT | ClearBuffer::FAST_CLEAR_BIT,
                            );

                            sys::sceGuDisable(GuState::Texture2D);
                            let bottom = SCREEN_HEIGHT as f32 - 40.0f32;
                            let width_avail = SCREEN_WIDTH as f32 - margin * 2.0f32;
                            let max_h = (SCREEN_HEIGHT as f32) * 0.5f32;

                            let verts_count = (display_m * 2) as i32;
                            // ignore this please
                            let vertices = core::ptr::addr_of_mut!(VERTEX_BUFFER.0) as *mut u8
                                as *mut ColVertex;

                            for i in 0..display_m {
                                let t = local[i] as f32;
                                let bar_h = (t.max(0.0) * max_h) as f32;
                                let x = precomputed_xs[i];
                                let y = bottom - bar_h;

                                let base = (i * 2) as isize;
                                let color = 0xFFFFFFFFu32;
                                ptr::write(
                                    vertices.offset(base),
                                    ColVertex {
                                        color,
                                        x: x,
                                        y: y,
                                        z: 0.0,
                                    },
                                );
                                ptr::write(
                                    vertices.offset(base + 1),
                                    ColVertex {
                                        color,
                                        x: x + cell_w * 0.9,
                                        y: y + bar_h,
                                        z: 0.0,
                                    },
                                );
                            }

                            sys::sceGuDrawArray(
                                GuPrimitive::Sprites,
                                VertexType::COLOR_8888
                                    | VertexType::VERTEX_32BITF
                                    | VertexType::TRANSFORM_2D,
                                verts_count,
                                ptr::null_mut(),
                                vertices as *const c_void,
                            );

                            sys::sceGuFinish();
                            sys::sceGuSync(GuSyncMode::Finish, GuSyncBehavior::Wait);
                            sys::sceDisplayWaitVblankStart();
                            sys::sceGuSwapBuffers();
                        }
                    }
                    Ok(false) => {
                        psp::dprintln!("MP3 finished");
                        SPECTRUM_STOP.store(true, Ordering::Relaxed);
                        if fft_thid.0 >= 0 {
                            let _ =
                                unsafe { sys::sceKernelWaitThreadEnd(fft_thid, ptr::null_mut()) };
                            let _ = unsafe { sys::sceKernelDeleteThread(fft_thid) };
                        }
                        break;
                    }
                    Err(_) => {
                        psp::dprintln!("MP3 error");
                        break;
                    }
                }

                // unsafe { sys::sceKernelDelayThreadCB(5000) };
            }
        }
        Err(_) => {
            psp::dprintln!("Failed to start MP3");
        }
    }

    psp::dprintln!("musializer-psp: exiting");
}

unsafe fn init_gu() {
    let allocator = get_vram_allocator().unwrap();

    let fbp0 = allocator.alloc_texture_pixels(
        BUF_WIDTH as u32,
        SCREEN_HEIGHT as u32,
        psp::sys::TexturePixelFormat::Psm8888,
    );
    let fbp1 = allocator.alloc_texture_pixels(
        BUF_WIDTH as u32,
        SCREEN_HEIGHT as u32,
        psp::sys::TexturePixelFormat::Psm8888,
    );
    let zbp = allocator.alloc_texture_pixels(
        BUF_WIDTH as u32,
        SCREEN_HEIGHT as u32,
        psp::sys::TexturePixelFormat::Psm4444,
    );

    let white_tex =
        allocator.alloc_texture_pixels(1u32, 1u32, psp::sys::TexturePixelFormat::Psm8888);
    unsafe {
        let p_direct = white_tex.as_mut_ptr_direct_to_vram() as *mut u32;
        p_direct.write_volatile(0xFFFFFFFFu32);
        WHITE_VRAM_PTR = white_tex.as_mut_ptr_from_zero() as *mut c_void;
    }

    unsafe {
        sys::sceGuInit();

        sys::sceGuStart(
            psp::sys::GuContextType::Direct,
            &raw mut LIST.0 as *mut _ as *mut c_void,
        );
        sys::sceGuDrawBuffer(
            psp::sys::DisplayPixelFormat::Psm8888,
            fbp0.as_mut_ptr_from_zero() as _,
            BUF_WIDTH as i32,
        );
        sys::sceGuDispBuffer(
            SCREEN_WIDTH as i32,
            SCREEN_HEIGHT as i32,
            fbp1.as_mut_ptr_from_zero() as _,
            BUF_WIDTH as i32,
        );
        sys::sceGuDepthBuffer(zbp.as_mut_ptr_from_zero() as _, BUF_WIDTH as i32);
        sys::sceGuOffset(2048 - (SCREEN_WIDTH / 2), 2048 - (SCREEN_HEIGHT / 2));
        sys::sceGuViewport(2048, 2048, SCREEN_WIDTH as i32, SCREEN_HEIGHT as i32);
        sys::sceGuDepthRange(65535, 0);
        sys::sceGuScissor(0, 0, SCREEN_WIDTH as i32, SCREEN_HEIGHT as i32);
        sys::sceGuEnable(psp::sys::GuState::ScissorTest);
        sys::sceGuEnable(psp::sys::GuState::Texture2D);
        sys::sceGuTexFunc(
            psp::sys::TextureEffect::Replace,
            psp::sys::TextureColorComponent::Rgba,
        );
        sys::sceGuEnable(psp::sys::GuState::Blend);
        sys::sceGuBlendFunc(
            psp::sys::BlendOp::Add,
            psp::sys::BlendFactor::SrcAlpha,
            psp::sys::BlendFactor::OneMinusSrcAlpha,
            0,
            0,
        );
        sys::sceGuAlphaFunc(psp::sys::AlphaFunc::Greater, 0, 0xff);
        sys::sceGuFinish();
        sys::sceGuSync(psp::sys::GuSyncMode::Finish, psp::sys::GuSyncBehavior::Wait);

        sys::sceDisplayWaitVblankStart();
        sys::sceGuDisplay(true);
    }
}

extern "C" fn fft_thread_main(_args: usize, argp: *mut c_void) -> i32 {
    let args_ptr = unsafe { *(argp as *const *mut c_void) } as *mut FftArgs;
    let args_box = unsafe { Box::from_raw(args_ptr) };
    let shared_ptr = args_box.shared_ptr;
    let analyzer_ptr = args_box.analyzer as *mut Analyzer;

    let mut samples = vec![0.0f32; fft::FFT_SIZE].into_boxed_slice();

    loop {
        if SPECTRUM_STOP.load(Ordering::Relaxed) {
            break;
        }

        let _ = mp3::snapshot_from_shared(shared_ptr, &mut samples);

        let analyzer = unsafe { &mut *analyzer_ptr };
        let m = analyzer.analyze(&samples, 1.0 / 60.0);
        let display_m = if m > SPECTRUM_SIZE { SPECTRUM_SIZE } else { m };

        unsafe {
            for i in 0..display_m {
                SPECTRUM.0[i] = analyzer.out_smooth[i];
            }
            for i in display_m..SPECTRUM_SIZE {
                SPECTRUM.0[i] = 0.0;
            }
        }
        SPECTRUM_GEN.fetch_add(1, Ordering::Release);

        // unsafe { sys::sceKernelDelayThreadCB(33333) };
    }

    unsafe { drop(Box::from_raw(analyzer_ptr)) };

    0
}

#[repr(C)]
struct FftArgs {
    shared_ptr: *mut core::ffi::c_void,
    analyzer: *mut Analyzer,
}
