//! The classes an element is given, and the style sheet a class is from.
//!
//! A view's CSS is a CSS module (`css_module!`): each class in it is a
//! [`Class`] constant, whose name no other sheet has, and which knows its
//! [`Sheet`]. A class that is not from a module (one the page's own sheet
//! has) is a string.

/// A CSS module's sheet as the document takes it.
#[derive(Debug)]
pub struct Sheet {
    pub(crate) id: &'static str,
    pub(crate) css: &'static str,
}

impl Sheet {
    #[doc(hidden)]
    pub const fn new(id: &'static str, css: &'static str) -> Self {
        Self { id, css }
    }
}

/// A class of a CSS module.
#[derive(Clone, Copy, Debug)]
pub struct Class {
    name: &'static str,
    sheet: &'static Sheet,
}

impl Class {
    #[doc(hidden)]
    pub const fn new(name: &'static str, sheet: &'static Sheet) -> Self {
        Self { name, sheet }
    }

    /// The class as the document has it.
    pub fn name(self) -> &'static str {
        self.name
    }
}

/// What an element can be given as its classes: a module's class, a
/// string of the page's own, or several of either.
pub trait Classes {
    /// Hands over each class, with its sheet if it is a module's.
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>));
}

impl Classes for Class {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        take(self.name, Some(self.sheet));
    }
}

impl Classes for str {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        for class in self.split_whitespace() {
            take(class, None);
        }
    }
}

impl Classes for String {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        self.as_str().each(take);
    }
}

impl<T: Classes + ?Sized> Classes for &T {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        (**self).each(take);
    }
}

impl<T: Classes> Classes for Option<T> {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        if let Some(classes) = self {
            classes.each(take);
        }
    }
}

impl<T: Classes, const N: usize> Classes for [T; N] {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        self.as_slice().each(take);
    }
}

impl<T: Classes> Classes for [T] {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        for classes in self {
            classes.each(take);
        }
    }
}

impl<T: Classes> Classes for Vec<T> {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        self.as_slice().each(take);
    }
}

impl<A: Classes, B: Classes> Classes for (A, B) {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        self.0.each(take);
        self.1.each(take);
    }
}

impl<A: Classes, B: Classes, C: Classes> Classes for (A, B, C) {
    fn each(&self, take: &mut dyn FnMut(&str, Option<&'static Sheet>)) {
        self.0.each(take);
        self.1.each(take);
        self.2.each(take);
    }
}
