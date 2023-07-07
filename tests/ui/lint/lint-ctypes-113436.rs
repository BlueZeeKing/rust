#![deny(improper_ctypes_definitions)]

#[repr(C)]
pub struct Wrap<T>(T);

#[repr(transparent)]
pub struct TransparentWrap<T>(T);

pub extern "C" fn f() -> Wrap<()> {
    //~^ ERROR `extern` fn uses type `()`, which is not FFI-safe
    todo!()
}

const _: extern "C" fn() -> Wrap<()> = f;
//~^ ERROR `extern` fn uses type `()`, which is not FFI-safe

pub extern "C" fn ff() -> Wrap<Wrap<()>> {
    //~^ ERROR `extern` fn uses type `()`, which is not FFI-safe
    todo!()
}

const _: extern "C" fn() -> Wrap<Wrap<()>> = ff;
//~^ ERROR `extern` fn uses type `()`, which is not FFI-safe

pub extern "C" fn g() -> TransparentWrap<()> {
    todo!()
}

const _: extern "C" fn() -> TransparentWrap<()> = g;

pub extern "C" fn gg() -> TransparentWrap<TransparentWrap<()>> {
    todo!()
}

const _: extern "C" fn() -> TransparentWrap<TransparentWrap<()>> = gg;

fn main() {}
