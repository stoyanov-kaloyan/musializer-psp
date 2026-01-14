#![no_std]
#![no_main]

extern crate alloc;

mod mp3;
mod utils;

use core::{ffi::c_void, ptr};
use mp3::Mp3Player;
use psp::sys;
use psp::vram_alloc::get_vram_allocator;
use psp::{Align16, BUF_WIDTH, SCREEN_HEIGHT, SCREEN_WIDTH};

// static GU list buffer
static mut LIST: Align16<[u32; 0x40000]> = Align16([0; 0x40000]);

// 1x1 white pixel for drawing colored rectangles
static WHITE_PIXEL: Align16<[u32; 1]> = Align16([0xFFFFFFFFu32]);

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

            loop {
                match player.tick() {
                    Ok(true) => {
                        let lvl = player.level();
                        // draw frame with GU
                        unsafe {
                            use psp::sys::ClearBuffer;
                            use psp::sys::GuContextType;
                            use psp::sys::GuPrimitive;
                            use psp::sys::GuState;
                            use psp::sys::GuSyncBehavior;
                            use psp::sys::GuSyncMode;
                            use psp::sys::MipmapLevel;
                            use psp::sys::TextureFilter;
                            use psp::sys::TexturePixelFormat;
                            use psp::sys::VertexType;

                            #[repr(C, align(4))]
                            struct TexVertex {
                                u: f32,
                                v: f32,
                                x: f32,
                                y: f32,
                                z: f32,
                            }

                            sys::sceGuStart(
                                GuContextType::Direct,
                                &raw mut LIST.0 as *mut _ as *mut c_void,
                            );
                            sys::sceGuClearColor(0xFF000000);
                            sys::sceGuClearDepth(0);
                            sys::sceGuClear(
                                ClearBuffer::COLOR_BUFFER_BIT | ClearBuffer::FAST_CLEAR_BIT,
                            );

                            // prepare 1x1 white texture
                            sys::sceGuEnable(GuState::Texture2D);
                            sys::sceGuTexMode(TexturePixelFormat::Psm8888, 0, 0, 0);
                            sys::sceGuTexImage(
                                MipmapLevel::None,
                                1,
                                1,
                                1,
                                WHITE_PIXEL.0.as_ptr() as *const c_void,
                            );
                            sys::sceGuTexScale(1.0, 1.0);
                            sys::sceGuTexFilter(TextureFilter::Nearest, TextureFilter::Nearest);
                            sys::sceGuTexWrap(
                                psp::sys::GuTexWrapMode::Clamp,
                                psp::sys::GuTexWrapMode::Clamp,
                            );
                            sys::sceGuTexFunc(
                                psp::sys::TextureEffect::Replace,
                                psp::sys::TextureColorComponent::Rgba,
                            );
                            sys::sceGuTexFlush();

                            // rectangle dimensions
                            let margin = 20.0f32;
                            let meter_w =
                                (SCREEN_WIDTH as f32 - margin * 2.0) * (lvl as f32 / 100.0);
                            let meter_h = 24.0f32;
                            let x = margin;
                            let y = (SCREEN_HEIGHT as f32) / 2.0 - meter_h / 2.0;

                            let vertices =
                                sys::sceGuGetMemory((2 * core::mem::size_of::<TexVertex>()) as i32)
                                    as *mut TexVertex;

                            ptr::write(
                                vertices,
                                TexVertex {
                                    u: 0.0,
                                    v: 0.0,
                                    x: x,
                                    y: y,
                                    z: 0.0,
                                },
                            );
                            ptr::write(
                                vertices.add(1),
                                TexVertex {
                                    u: 1.0,
                                    v: 1.0,
                                    x: x + meter_w,
                                    y: y + meter_h,
                                    z: 0.0,
                                },
                            );

                            sys::sceGuDrawArray(
                                GuPrimitive::Sprites,
                                VertexType::TEXTURE_32BITF
                                    | VertexType::VERTEX_32BITF
                                    | VertexType::TRANSFORM_2D,
                                2,
                                ptr::null_mut(),
                                vertices as *const c_void,
                            );

                            sys::sceGuDisable(GuState::Texture2D);

                            sys::sceGuFinish();
                            sys::sceGuSync(GuSyncMode::Finish, GuSyncBehavior::Wait);
                            sys::sceDisplayWaitVblankStart();
                            sys::sceGuSwapBuffers();
                        }
                    }
                    Ok(false) => {
                        psp::dprintln!("MP3 finished");
                        break;
                    }
                    Err(_) => {
                        psp::dprintln!("MP3 error");
                        break;
                    }
                }

                unsafe { sys::sceKernelDelayThreadCB(5000) };
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
