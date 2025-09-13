pub trait UUnwrap {
    type Inner;
    fn uunwrap(self) -> Self::Inner;
}

impl<T> UUnwrap for Option<T> {
    type Inner = T;
    #[inline(always)]
    fn uunwrap(self) -> Self::Inner {
        match self {
            Some(v) => v,
            None => bang(),
        }
    }
}

impl<T, U> UUnwrap for Result<T, U> {
    type Inner = T;
    #[inline(always)]
    fn uunwrap(self) -> Self::Inner {
        match self {
            Ok(v) => v,
            Err(_) => bang(),
        }
    }
}
#[cold]
#[inline(never)]
fn bang() -> ! {
    panic!("bang")
}

