//! A plugin described once: this module is all there is of its shape. Its
//! WIT world, its exports and its manifest are made from it
//! (`plugin-macro`).

#[plugin_macro::plugin(name = "notes", title = "Notes", read = ["/"])]
mod notes {
    use std::cell::RefCell;

    /// What a pane keeps: the file it shows, and the line it is at.
    pub struct State {
        pub path: Option<String>,
        pub line: u32,
    }

    thread_local! {
        static SHOWN: RefCell<(Option<String>, u32)> = const { RefCell::new((None, 0)) };
        static BEFORE: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Stands for building the view: the node it is rooted at.
    pub fn mount(state: Option<State>) -> u64 {
        let shown = state.map_or((None, 0), |state| (state.path, state.line));
        SHOWN.with(|kept| *kept.borrow_mut() = shown);
        7
    }

    pub fn state() -> State {
        let (path, line) = SHOWN.with(|kept| kept.borrow().clone());
        State { path, line }
    }

    #[create(file(extensions = ["md", "markdown"]))]
    pub fn create(file: File) -> State {
        State {
            path: Some(file.path()),
            line: 0,
        }
    }

    /// Stands for following a link: not a command, so not exported.
    #[allow(dead_code)]
    fn follow(to: String) {
        let from = SHOWN.with(|kept| kept.borrow_mut().0.replace(to));
        BEFORE.with(|before| before.borrow_mut().extend(from));
    }

    #[command(title = "Back", description = "Go back to the file a link was followed from.")]
    pub fn back() {
        if let Some(before) = BEFORE.with(|before| before.borrow_mut().pop()) {
            SHOWN.with(|kept| kept.borrow_mut().0 = Some(before));
        }
    }

    #[command(title = "Go to Line", description = "Move to a line of the file.")]
    pub fn go_to_line(line: u32) {
        SHOWN.with(|kept| kept.borrow_mut().1 = line);
    }

    #[command(title = "Forget", description = "Forget the file shown.", destructive)]
    pub fn forget() {
        SHOWN.with(|kept| *kept.borrow_mut() = (None, 0));
    }
}
