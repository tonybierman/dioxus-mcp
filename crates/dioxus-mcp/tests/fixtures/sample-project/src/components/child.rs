// Demonstrates: props_lint (ChildProps missing PartialEq),
//                project_index (Props-struct component)
use dioxus::prelude::*;

#[derive(Props, Clone)]
pub struct ChildProps {
    pub name: String,
    pub user_id: i32,
}

#[component]
pub fn Child(props: ChildProps) -> Element {
    rsx! { span { "{props.name}" } }
}
