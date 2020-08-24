use std::fmt::Display;
use std::io;

pub trait Context {
    fn context(self, message: impl Display) -> Self;
}

impl Context for io::Error {
    fn context(self, message: impl Display) -> Self {
        Self::new(self.kind(), format!("{}: {}", message, self))
    }
}

impl<T> Context for io::Result<T> {
    fn context(self, message: impl Display) -> Self {
        self.map_err(|e| e.context(message))
    }
}
