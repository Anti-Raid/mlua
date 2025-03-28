#![cfg(feature = "luau")]

use std::fmt::Debug;
use std::fs;
use std::os::raw::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::Arc;

use mlua::{Compiler, Error, Lua, LuaOptions, Result, StdLib, Table, ThreadStatus, Value, Vector, VmState};

#[test]
fn test_version() -> Result<()> {
    let lua = Lua::new();
    assert!(lua.globals().get::<String>("_VERSION")?.starts_with("Luau 0."));
    Ok(())
}

#[test]
fn test_require() -> Result<()> {
    // Ensure that require() is not available if package module is not loaded
    let mut lua = Lua::new_with(StdLib::NONE, LuaOptions::default())?;
    assert!(lua.globals().get::<Option<Value>>("require")?.is_none());
    assert!(lua.globals().get::<Option<Value>>("package")?.is_none());

    if cfg!(target_arch = "wasm32") {
        // TODO: figure out why emscripten fails on file operations
        // Also see https://github.com/rust-lang/rust/issues/119250
        return Ok(());
    }

    lua = Lua::new();

    // Check that require() can load stdlib modules (including `package`)
    lua.load(
        r#"
        local math = require("math")
        assert(math == _G.math, "math module does not match _G.math")
        local package = require("package")
        assert(package == _G.package, "package module does not match _G.package")
    "#,
    )
    .exec()?;

    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(
        temp_dir.path().join("module.luau"),
        r#"
        counter = (counter or 0) + 1
        return {
            counter = counter,
            error = function() error("test") end,
        }
    "#,
    )?;

    lua.globals()
        .get::<Table>("package")?
        .set("path", temp_dir.path().join("?.luau").to_string_lossy())?;

    lua.load(
        r#"
        local module = require("module")
        assert(module.counter == 1)
        module = require("module")
        assert(module.counter == 1)

        local ok, err = pcall(module.error)
        assert(not ok and string.find(err, "module.luau") ~= nil)
    "#,
    )
    .exec()?;

    // Require non-existent module
    match lua.load("require('non-existent')").exec() {
        Err(Error::RuntimeError(e)) if e.contains("module 'non-existent' not found") => {}
        r => panic!("expected RuntimeError(...) with a specific message, got {r:?}"),
    }

    // Require binary module in safe mode
    lua.globals()
        .get::<Table>("package")?
        .set("cpath", temp_dir.path().join("?.so").to_string_lossy())?;
    fs::write(temp_dir.path().join("dylib.so"), "")?;
    match lua.load("require('dylib')").exec() {
        Err(Error::RuntimeError(e)) if cfg!(unix) && e.contains("module 'dylib' not found") => {
            assert!(e.contains("dynamic libraries are disabled in safe mode"))
        }
        Err(Error::RuntimeError(e)) if e.contains("module 'dylib' not found") => {}
        r => panic!("expected RuntimeError(...) with a specific message, got {r:?}"),
    }

    Ok(())
}

#[cfg(not(feature = "luau-vector4"))]
#[test]
fn test_vectors() -> Result<()> {
    let lua = Lua::new();

    let v: Vector = lua
        .load("vector.create(1, 2, 3) + vector.create(3, 2, 1)")
        .eval()?;
    assert_eq!(v, [4.0, 4.0, 4.0]);

    // Test conversion into Rust array
    let v: [f64; 3] = lua.load("vector.create(1, 2, 3)").eval()?;
    assert!(v == [1.0, 2.0, 3.0]);

    // Test vector methods
    lua.load(
        r#"
        local v = vector.create(1, 2, 3)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
    "#,
    )
    .exec()?;

    // Test vector methods (fastcall)
    lua.load(
        r#"
        local v = vector.create(1, 2, 3)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
    "#,
    )
    .set_compiler(Compiler::new().set_vector_ctor("vector"))
    .exec()?;

    Ok(())
}

#[cfg(feature = "luau-vector4")]
#[test]
fn test_vectors() -> Result<()> {
    let lua = Lua::new();

    let v: Vector = lua
        .load("vector.create(1, 2, 3, 4) + vector.create(4, 3, 2, 1)")
        .eval()?;
    assert_eq!(v, [5.0, 5.0, 5.0, 5.0]);

    // Test conversion into Rust array
    let v: [f64; 4] = lua.load("vector.create(1, 2, 3, 4)").eval()?;
    assert!(v == [1.0, 2.0, 3.0, 4.0]);

    // Test vector methods
    lua.load(
        r#"
        local v = vector.create(1, 2, 3, 4)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
        assert(v.w == 4)
    "#,
    )
    .exec()?;

    // Test vector methods (fastcall)
    lua.load(
        r#"
        local v = vector.create(1, 2, 3, 4)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
        assert(v.w == 4)
    "#,
    )
    .set_compiler(Compiler::new().set_vector_ctor("vector"))
    .exec()?;

    Ok(())
}

#[cfg(not(feature = "luau-vector4"))]
#[test]
fn test_vector_metatable() -> Result<()> {
    let lua = Lua::new();

    let vector_mt = lua
        .load(
            r#"
            {
                __index = {
                    new = vector.create,

                    product = function(a, b)
                        return vector.create(a.x * b.x, a.y * b.y, a.z * b.z)
                    end
                }
            }
    "#,
        )
        .eval::<Table>()?;
    vector_mt.set_metatable(Some(vector_mt.clone()));
    lua.set_type_metatable::<Vector>(Some(vector_mt.clone()));
    lua.globals().set("Vector3", vector_mt)?;

    let compiler = Compiler::new().set_vector_lib("Vector3").set_vector_ctor("new");

    // Test vector methods (fastcall)
    lua.load(
        r#"
        local v = Vector3.new(1, 2, 3)
        local v2 = v:product(Vector3.new(2, 3, 4))
        assert(v2.x == 2 and v2.y == 6 and v2.z == 12)
    "#,
    )
    .set_compiler(compiler)
    .exec()?;

    Ok(())
}

#[test]
fn test_readonly_table() -> Result<()> {
    let lua = Lua::new();

    let t = lua.create_sequence_from([1])?;
    assert!(!t.is_readonly());
    t.set_readonly(true);
    assert!(t.is_readonly());

    #[track_caller]
    fn check_readonly_error<T: Debug>(res: Result<T>) {
        match res {
            Err(Error::RuntimeError(e)) if e.contains("attempt to modify a readonly table") => {}
            r => panic!("expected RuntimeError(...) with a specific message, got {r:?}"),
        }
    }

    check_readonly_error(t.set("key", "value"));
    check_readonly_error(t.raw_set("key", "value"));
    check_readonly_error(t.raw_insert(1, "value"));
    check_readonly_error(t.raw_remove(1));
    check_readonly_error(t.push("value"));
    check_readonly_error(t.pop::<Value>());
    check_readonly_error(t.raw_push("value"));
    check_readonly_error(t.raw_pop::<Value>());

    // Special case
    match catch_unwind(AssertUnwindSafe(|| t.set_metatable(None))) {
        Ok(_) => panic!("expected panic, got nothing"),
        Err(_) => {}
    }

    Ok(())
}

#[test]
fn test_sandbox() -> Result<()> {
    let lua = Lua::new();

    lua.sandbox(true)?;

    lua.load("global = 123").exec()?;
    let n: i32 = lua.load("return global").eval()?;
    assert_eq!(n, 123);
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(123));

    // Threads should inherit "main" globals
    let f = lua.create_function(|lua, ()| lua.globals().get::<i32>("global"))?;
    let co = lua.create_thread(f.clone())?;
    assert_eq!(co.resume::<Option<i32>>(())?, Some(123));

    // Sandboxed threads should also inherit "main" globals
    let co = lua.create_thread(f)?;
    co.sandbox()?;
    assert_eq!(co.resume::<Option<i32>>(())?, Some(123));

    lua.sandbox(false)?;

    // Previously set variable `global` should be cleared now
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, None);

    // Readonly flags should be cleared as well
    let table = lua.globals().get::<Table>("table")?;
    table.set("test", "test")?;

    Ok(())
}

#[test]
fn test_sandbox_safeenv() -> Result<()> {
    let lua = Lua::new();

    lua.sandbox(true)?;
    lua.globals().set("state", lua.create_table()?)?;
    lua.globals().set_safeenv(false);
    lua.load("state.a = 123").exec()?;
    let a: i32 = lua.load("state.a = 321; return state.a").eval()?;
    assert_eq!(a, 321);

    Ok(())
}

#[test]
fn test_sandbox_nolibs() -> Result<()> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).unwrap();

    lua.sandbox(true)?;
    lua.load("global = 123").exec()?;
    let n: i32 = lua.load("return global").eval()?;
    assert_eq!(n, 123);
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(123));

    lua.sandbox(false)?;
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, None);

    Ok(())
}

#[test]
fn test_sandbox_threads() -> Result<()> {
    let lua = Lua::new();

    let f = lua.create_function(|lua, v: Value| lua.globals().set("global", v))?;

    let co = lua.create_thread(f.clone())?;
    co.resume::<()>(321)?;
    // The main state should see the `global` variable (as the thread is not sandboxed)
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(321));

    let co = lua.create_thread(f.clone())?;
    co.sandbox()?;
    co.resume::<()>(123)?;
    // The main state should see the previous `global` value (as the thread is sandboxed)
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(321));

    // Try to reset the (sandboxed) thread
    co.reset(f)?;
    co.resume::<()>(111)?;
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(111));

    Ok(())
}

#[test]
fn test_interrupts() -> Result<()> {
    let lua = Lua::new();

    let interrupts_count = Arc::new(AtomicU64::new(0));
    let interrupts_count2 = interrupts_count.clone();

    lua.set_interrupt(move |_| {
        interrupts_count2.fetch_add(1, Ordering::Relaxed);
        Ok(VmState::Continue)
    });
    let f = lua
        .load(
            r#"
        local x = 2 + 3
        local y = x * 63
        local z = string.len(x..", "..y)
    "#,
        )
        .into_function()?;
    f.call::<()>(())?;

    assert!(interrupts_count.load(Ordering::Relaxed) > 0);

    //
    // Test yields from interrupt
    //
    let yield_count = Arc::new(AtomicU64::new(0));
    let yield_count2 = yield_count.clone();
    lua.set_interrupt(move |_| {
        if yield_count2.fetch_add(1, Ordering::Relaxed) == 1 {
            return Ok(VmState::Yield);
        }
        Ok(VmState::Continue)
    });
    let co = lua.create_thread(
        lua.load(
            r#"
            local a = {1, 2, 3}
            local b = 0
            for _, x in ipairs(a) do b += x end
            return b
        "#,
        )
        .into_function()?,
    )?;
    co.resume::<()>(())?;
    assert_eq!(co.status(), ThreadStatus::Resumable);
    let result: i32 = co.resume(())?;
    assert_eq!(result, 6);
    assert_eq!(yield_count.load(Ordering::Relaxed), 7);
    assert_eq!(co.status(), ThreadStatus::Finished);

    //
    // Test errors in interrupts
    //
    lua.set_interrupt(|_| Err(Error::runtime("error from interrupt")));
    match f.call::<()>(()) {
        Err(Error::RuntimeError(ref msg)) => assert_eq!(msg, "error from interrupt"),
        res => panic!("expected `RuntimeError` with a specific message, got {res:?}"),
    }

    lua.remove_interrupt();

    Ok(())
}

#[test]
fn test_fflags() {
    // We cannot really on any particular feature flag to be present
    assert!(Lua::set_fflag("UnknownFlag", true).is_err());
}

#[test]
fn test_thread_events() -> Result<()> {
    let lua = Lua::new();

    let count = Arc::new(AtomicU64::new(0));
    let thread_data: Arc<(AtomicPtr<c_void>, AtomicBool)> = Arc::new(Default::default());

    let (count2, thread_data2) = (count.clone(), thread_data.clone());
    lua.set_thread_event_callback(move |_, value| {
        count2.fetch_add(1, Ordering::Relaxed);
        (thread_data2.0).store(value.to_pointer() as *mut _, Ordering::Relaxed);
        if value.is_thread() {
            thread_data2.1.store(false, Ordering::Relaxed);
        }
        if value.is_light_userdata() {
            thread_data2.1.store(true, Ordering::Relaxed);
        }
        Ok(())
    });

    let t = lua.create_thread(lua.load("return 123").into_function()?)?;
    assert_eq!(count.load(Ordering::Relaxed), 1);
    let t_ptr = t.to_pointer();
    assert_eq!(t_ptr, thread_data.0.load(Ordering::Relaxed));
    assert!(!thread_data.1.load(Ordering::Relaxed));

    // Thead will be destroyed after GC cycle
    drop(t);
    lua.gc_collect()?;
    assert_eq!(count.load(Ordering::Relaxed), 2);
    assert_eq!(t_ptr, thread_data.0.load(Ordering::Relaxed));
    assert!(thread_data.1.load(Ordering::Relaxed));

    // Check that recursion is not allowed
    let count3 = count.clone();
    lua.set_thread_event_callback(move |lua, _value| {
        count3.fetch_add(1, Ordering::Relaxed);
        let _ = lua.create_thread(lua.load("return 123").into_function().unwrap())?;
        Ok(())
    });
    let t = lua.create_thread(lua.load("return 123").into_function()?)?;
    assert_eq!(count.load(Ordering::Relaxed), 3);

    lua.remove_thread_event_callback();
    drop(t);
    lua.gc_collect()?;
    assert_eq!(count.load(Ordering::Relaxed), 3);

    // Test error inside callback
    lua.set_thread_event_callback(move |_, _| Err(Error::runtime("error when processing thread event")));
    let result = lua.create_thread(lua.load("return 123").into_function()?);
    assert!(result.is_err());
    assert!(
        matches!(result, Err(Error::RuntimeError(err)) if err.contains("error when processing thread event"))
    );

    Ok(())
}
