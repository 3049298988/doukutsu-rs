#[macro_use]
extern crate log;
#[cfg_attr(feature = "scripting", macro_use)]
#[cfg(feature = "scripting")]
extern crate lua_ffi;
extern crate strum;
#[macro_use]
extern crate strum_macros;

use std::env;
use std::cell::UnsafeCell;
use std::time::Instant;

use log::*;
use pretty_env_logger::env_logger::Env;

use crate::framework::context::Context;
use crate::framework::error::{GameError, GameResult};
use crate::framework::graphics;
use crate::framework::keyboard::ScanCode;
use crate::scene::loading_scene::LoadingScene;
use crate::scene::Scene;
use crate::shared_game_state::{SharedGameState, TimingMode};
use crate::texture_set::G_MAG;
use crate::ui::UI;

mod bmfont;
mod bmfont_renderer;
mod builtin_fs;
mod bullet;
mod caret;
mod common;
mod components;
mod difficulty_modifier;
mod encoding;
mod engine_constants;
mod entity;
mod frame;
mod framework;
mod inventory;
mod input;
mod live_debugger;
mod macros;
mod map;
mod menu;
mod npc;
mod physics;
mod player;
mod profile;
mod rng;
mod scene;
#[cfg(feature = "scripting")]
mod scripting;
mod settings;
#[cfg(feature = "backend-gfx")]
mod shaders;
mod shared_game_state;
mod stage;
mod sound;
mod text_script;
mod texture_set;
mod ui;
mod weapon;

struct Game {
    scene: Option<Box<dyn Scene>>,
    state: UnsafeCell<SharedGameState>,
    ui: UI,
    start_time: Instant,
    last_tick: u128,
    next_tick: u128,
    loops: u64,
}

impl Game {
    fn new(ctx: &mut Context) -> GameResult<Game> {
        let s = Game {
            scene: None,
            ui: UI::new(ctx)?,
            state: UnsafeCell::new(SharedGameState::new(ctx)?),
            start_time: Instant::now(),
            last_tick: 0,
            next_tick: 0,
            loops: 0,
        };

        Ok(s)
    }

    fn update(&mut self, ctx: &mut Context) -> GameResult {
        if let Some(scene) = self.scene.as_mut() {
            let state_ref = unsafe { &mut *self.state.get() };

            match state_ref.timing_mode {
                TimingMode::_50Hz | TimingMode::_60Hz => {
                    let last_tick = self.next_tick;

                    while self.start_time.elapsed().as_nanos() >= self.next_tick && self.loops < 10 {
                        if (state_ref.settings.speed - 1.0).abs() < 0.01 {
                            self.next_tick += state_ref.timing_mode.get_delta() as u128;
                        } else {
                            self.next_tick += (state_ref.timing_mode.get_delta() as f64 / state_ref.settings.speed) as u128;
                        }
                        self.loops += 1;
                    }

                    if self.loops == 10 {
                        log::warn!("Frame skip is way too high, a long system lag occurred?");
                        self.last_tick = self.start_time.elapsed().as_nanos();
                        self.next_tick = self.last_tick + (state_ref.timing_mode.get_delta() as f64 / state_ref.settings.speed) as u128;
                        self.loops = 0;
                    }

                    if self.loops != 0 {
                        scene.draw_tick(state_ref)?;
                        self.last_tick = last_tick;
                    }

                    for _ in 0..self.loops {
                        scene.tick(state_ref, ctx)?;
                    }
                }
                TimingMode::FrameSynchronized => {
                    scene.tick(state_ref, ctx)?;
                }
            }
        }
        Ok(())
    }

    fn draw(&mut self, ctx: &mut Context) -> GameResult {
        let state_ref = unsafe { &mut *self.state.get() };

        if state_ref.timing_mode != TimingMode::FrameSynchronized {
            let mut elapsed = self.start_time.elapsed().as_nanos();
            #[cfg(target_os = "windows")]
                {
                    // Even with the non-monotonic Instant mitigation at the start of the event loop, there's still a chance of it not working.
                    // This check here should trigger if that happens and makes sure there's no panic from an underflow.
                    if elapsed < self.last_tick {
                        elapsed = self.last_tick;
                    }
                }
            let n1 = (elapsed - self.last_tick) as f64;
            let n2 = (self.next_tick - self.last_tick) as f64;
            state_ref.frame_time = if state_ref.settings.motion_interpolation {
                n1 / n2
            } else { 1.0 };
        }
        unsafe { G_MAG = if state_ref.settings.subpixel_coords { state_ref.scale } else { 1.0 } };
        self.loops = 0;

        graphics::clear(ctx, [0.0, 0.0, 0.0, 1.0].into());
        /*graphics::set_projection(ctx, DrawParam::new()
            .scale(Vec2::new(state_ref.scale, state_ref.scale))
            .to_matrix());*/

        if let Some(scene) = self.scene.as_mut() {
            scene.draw(state_ref, ctx)?;
            if state_ref.settings.touch_controls {
                state_ref.touch_controls.draw(state_ref.canvas_size, &state_ref.constants, &mut state_ref.texture_set, ctx)?;
            }

            //graphics::set_projection(ctx, self.def_matrix);
            self.ui.draw(state_ref, ctx, scene)?;
        }

        graphics::present(ctx)?;
        Ok(())
    }

    fn key_down_event(&mut self, key_code: ScanCode, repeat: bool) {
        if repeat { return; }

        let state = unsafe { &mut *self.state.get() };
        match key_code {
            ScanCode::F5 => { state.settings.subpixel_coords = !state.settings.subpixel_coords }
            ScanCode::F6 => { state.settings.motion_interpolation = !state.settings.motion_interpolation }
            ScanCode::F7 => { state.set_speed(1.0) }
            ScanCode::F8 => {
                if state.settings.speed > 0.2 {
                    state.set_speed(state.settings.speed - 0.1);
                }
            }
            ScanCode::F9 => {
                if state.settings.speed < 3.0 {
                    state.set_speed(state.settings.speed + 0.1);
                }
            }
            ScanCode::F10 => { state.settings.debug_outlines = !state.settings.debug_outlines }
            ScanCode::F11 => { state.settings.god_mode = !state.settings.god_mode }
            ScanCode::F12 => { state.settings.infinite_booster = !state.settings.infinite_booster }
            _ => {}
        }
    }
}

#[cfg(target_os = "android")]
fn request_perms() -> GameResult {
    use jni::objects::JValue;
    use jni::objects::JObject;

    let native_activity = ndk_glue::native_activity();
    let vm_ptr = native_activity.vm();
    let vm = unsafe { jni::JavaVM::from_raw(vm_ptr) }?;
    let vm_env = vm.attach_current_thread()?;

    fn perm_name<'a, 'b, 'c>(vm_env: &'b jni::AttachGuard<'a>, name: &'c str) -> GameResult<jni::objects::JValue<'a>> {
        let class = vm_env.find_class("android/Manifest$permission")?;
        Ok(vm_env.get_static_field(class, name.to_owned(), "Ljava/lang/String;")?)
    }

    fn has_permission(vm_env: &jni::AttachGuard, activity: &jni::sys::jobject, name: &str) -> GameResult<bool> {
        let perm_granted = {
            let class = vm_env.find_class("android/content/pm/PackageManager")?;
            vm_env.get_static_field(class, "PERMISSION_GRANTED", "I")?.i()?
        };

        let perm = perm_name(vm_env, name)?;
        let activity_obj = JObject::from(*activity);
        let result = vm_env.call_method(activity_obj, "checkSelfPermission", "(Ljava/lang/String;)I", &[perm])?.i()?;
        Ok(result == perm_granted)
    }

    let str_class = vm_env.find_class("java/lang/String")?;
    let array = vm_env.new_object_array(2, str_class, JObject::null())?;
    vm_env.set_object_array_element(array, 0, perm_name(&vm_env, "READ_EXTERNAL_STORAGE")?.l()?)?;
    vm_env.set_object_array_element(array, 1, perm_name(&vm_env, "WRITE_EXTERNAL_STORAGE")?.l()?)?;
    let activity_obj = JObject::from(native_activity.activity());

    loop {
        if has_permission(&vm_env, &native_activity.activity(), "READ_EXTERNAL_STORAGE")?
            && has_permission(&vm_env, &native_activity.activity(), "WRITE_EXTERNAL_STORAGE")? {
            break;
        }

        vm_env.call_method(activity_obj, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::from(array), JValue::from(0)])?;
    }

    Ok(())
}

#[cfg(target_os = "android")]
#[cfg_attr(target_os = "android", ndk_glue::main(backtrace = "on"))]
pub fn android_main() {
    println!("main invoked.");

    request_perms().expect("Failed to attach to the JVM and request storage permissions.");

    env::set_var("CAVESTORY_DATA_DIR", "/sdcard/doukutsu");

    let _ = std::fs::create_dir("/sdcard/doukutsu/");
    let _ = std::fs::write("/sdcard/doukutsu/.nomedia", b"");

    init().unwrap();
}

pub fn init() -> GameResult {
    pretty_env_logger::env_logger::from_env(Env::default().default_filter_or("info"))
        .init();

    let mut resource_dir = env::current_exe()?;

    // Ditch the filename (if any)
    if resource_dir.file_name().is_some() {
        let _ = resource_dir.pop();
    }
    resource_dir.push("data");

    info!("Resource directory: {:?}", resource_dir);
    info!("Initializing engine...");

    let mut context: Context = Context::new();

    #[cfg(target_os = "android")]
        {
            loop {
                match ndk_glue::native_window().as_ref() {
                    Some(_) => {
                        println!("NativeScreen Found:{:?}", ndk_glue::native_window());
                        break;
                    }
                    None => ()
                }
            }
        }
}
