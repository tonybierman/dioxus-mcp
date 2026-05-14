// Demonstrates: route_map (layouts, nests, params), create_route target
use dioxus::prelude::*;
use crate::components::*;

#[derive(Routable, Clone, PartialEq)]
pub enum Route {
    #[layout(NavBar)]
        #[route("/")]
        Home {},
        #[route("/user/:id")]
        UserPage { id: i32 },
        #[nest("/blog")]
            #[route("/")]
            BlogIndex {},
            #[route("/:slug")]
            BlogPost { slug: String },
        #[end_nest]
    #[end_layout]
    #[route("/about")]
    About {},
}
