use std::alloc::Layout;

pub struct Dropper {
    pub alloc: *mut u8,
    pub layout: Layout,
}

impl Drop for Dropper {
    fn drop(&mut self) {
        unsafe {
            std::alloc::dealloc(self.alloc, self.layout);
        }
    }
}