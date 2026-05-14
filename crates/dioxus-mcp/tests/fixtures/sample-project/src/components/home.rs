// Demonstrates: explain_signal_graph (use_signal + use_memo),
//                signal_lint (use_signal inside rsx! for loop),
//                prop_drill (props.title.clone() and props.user_id into Child),
//                project_index (Props-struct component)
use dioxus::prelude::*;
use crate::components::Child;

#[derive(Props, PartialEq, Clone)]
pub struct HomeProps {
    pub title: String,
    pub user_id: i32,
}

#[component]
pub fn Home(props: HomeProps) -> Element {
    let count = use_signal(|| 0);
    let doubled = use_memo(move || count() * 2);

    let items = vec![1, 2, 3];
    rsx! {
        div {
            h1 { "{props.title} ({doubled})" }
            Child { name: props.title.clone(), user_id: props.user_id }
            for i in items.iter() {
                let _per_item = use_signal(|| 0);
                div { key: "{i}", "item {i}" }
            }
        }
    }
}
