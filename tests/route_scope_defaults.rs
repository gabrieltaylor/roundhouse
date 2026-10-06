//! `scope defaults: { user_id: "me" }` makes a segment's helper param
//! OPTIONAL (`lower::routes` → `routes_to_library::build_helper_function`).
//!
//! Rails fills a defaulted dynamic segment in when the caller omits it,
//! so campfire's `resource :profile` under that scope answers
//! `user_profile_url` with NO argument even though its path is
//! `/users/:user_id/profile`. Ingest used to drop `defaults:` as
//! something that "shapes the request, not the (path, controller,
//! action) triple" — true of the triple, false of the SIGNATURE, and
//! four of campfire's controller tests died on `wrong number of
//! arguments (given 0, expected 1)`.

use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::ingest::{ingest_app_from_tree, ingest_routes};
use roundhouse::lower::{flatten_routes, lower_routes_to_library_functions};
use roundhouse::ty::{ParamKind, Ty};

const SCHEMA: &str = "ActiveRecord::Schema.define(version: 1) do\n  \
    create_table :users do |t|\n    t.string :name\n  end\nend\n";

fn helper_params(routes: &str, name: &str) -> Vec<roundhouse::ty::Param> {
    let files = vec![
        ("db/schema.rb", SCHEMA),
        ("app/models/user.rb", "class User < ApplicationRecord\nend\n"),
        ("config/routes.rb", routes),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files
        .into_iter()
        .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let app = ingest_app_from_tree(tree).expect("ingest tree");
    let helpers = lower_routes_to_library_functions(&app);
    let f = helpers
        .iter()
        .find(|f| f.name.as_str() == name)
        .unwrap_or_else(|| {
            panic!(
                "helper {name} not generated; got: {:?}",
                helpers.iter().map(|f| f.name.as_str().to_string()).collect::<Vec<_>>()
            )
        });
    let Ty::Fn { params, .. } = f.signature.clone().expect("signature") else {
        panic!("not a Ty::Fn")
    };
    params
}

/// The shape campfire has, verbatim.
const CAMPFIRE_SHAPE: &str = r#"
Rails.application.routes.draw do
  resources :users, only: :show do
    scope module: "users" do
      scope defaults: { user_id: "me" } do
        resource :profile
      end
    end
  end
end
"#;

#[test]
fn a_defaulted_segment_is_an_optional_helper_param() {
    let params = helper_params(CAMPFIRE_SHAPE, "user_profile_path");
    assert_eq!(params.len(), 1, "the segment is still a parameter: {params:?}");
    assert_eq!(params[0].name.as_str(), "user_id");
    assert_eq!(
        params[0].kind,
        ParamKind::Optional,
        "a defaulted segment must be callable with no argument: {params:?}"
    );
}

#[test]
fn an_undefaulted_segment_stays_required() {
    let params = helper_params(
        r#"
Rails.application.routes.draw do
  resources :users, only: :show do
    scope module: "users" do
      resource :profile
    end
  end
end
"#,
        "user_profile_path",
    );
    assert_eq!(params.len(), 1);
    assert_eq!(
        params[0].kind,
        ParamKind::Required,
        "without `defaults:` the segment is still required: {params:?}"
    );
}

/// The default applies only inside the scope that declares it.
#[test]
fn the_default_does_not_leak_to_a_sibling_scope() {
    let params = helper_params(
        r#"
Rails.application.routes.draw do
  resources :users, only: :show do
    scope module: "users" do
      scope defaults: { user_id: "me" } do
        resource :profile
      end
      resource :avatar
    end
  end
end
"#,
        "user_avatar_path",
    );
    assert_eq!(params.len(), 1);
    assert_eq!(
        params[0].kind,
        ParamKind::Required,
        "the sibling resource declares no default: {params:?}"
    );
}

/// A defaulted segment that is NOT the SUFFIX becomes a KEYWORD, and
/// leaves the positional list entirely.
///
/// campfire routes push_subscriptions under the same scope, so Rails
/// accepts `user_push_subscription_path(record)` — one argument for a
/// two-segment member route. Keeping `user_id` a positional-with-a-
/// default is what Ruby would do and what the strict targets cannot:
/// Rust has no default arguments, so the emitter fills them at the CALL
/// SITE by padding MISSING TRAILING args, and a LEADING default would be
/// appended at the end instead of filled in place — silently swapping
/// the segments in the URL.
///
/// The cost, and the direction is the point: Rails also accepts
/// `user_push_subscription_path(user, record)`. Against a keyword that
/// is an ARITY ERROR — loud, at the call site, naming the helper —
/// rather than a URL with its segments swapped.
const NESTED_MEMBER_SHAPE: &str = r#"
Rails.application.routes.draw do
  resources :users, only: :show do
    scope module: "users" do
      scope defaults: { user_id: "me" } do
        resources :push_subscriptions
      end
    end
  end
end
"#;

#[test]
fn a_leading_defaulted_segment_becomes_a_keyword() {
    let params = helper_params(NESTED_MEMBER_SHAPE, "user_push_subscription_path");
    assert_eq!(params.len(), 2, "both segments are still parameters: {params:?}");
    assert_eq!(params[0].name.as_str(), "id", "the undefaulted segment leads: {params:?}");
    assert_eq!(params[0].kind, ParamKind::Required);
    assert_eq!(params[1].name.as_str(), "user_id");
    assert_eq!(
        params[1].kind,
        ParamKind::Keyword { required: false },
        "the defaulted segment is a keyword, not a positional: {params:?}"
    );
}

/// …and when the defaults ARE the suffix, nothing moves: the collection
/// helper under the same scope keeps its single optional positional.
#[test]
fn a_trailing_defaulted_segment_stays_positional() {
    let params = helper_params(NESTED_MEMBER_SHAPE, "user_push_subscriptions_path");
    assert_eq!(params.len(), 1, "{params:?}");
    assert_eq!(params[0].name.as_str(), "user_id");
    assert_eq!(params[0].kind, ParamKind::Optional, "{params:?}");
}

fn routes_app(body: &str) -> roundhouse::App {
    let source = format!("Rails.application.routes.draw do\n{body}\nend\n");
    roundhouse::App {
        routes: ingest_routes(source.as_bytes(), "config/routes.rb").expect("ingest routes"),
        ..Default::default()
    }
}

fn flat_routes(body: &str) -> Vec<roundhouse::lower::routes::FlatRoute> {
    flatten_routes(&routes_app(body))
}

#[test]
fn defaults_blocks_merge_restore_and_apply_to_every_route_kind() {
    let routes = flat_routes(
        r#"
  scope defaults: { format: :html } do
    defaults format: :json do
      root "posts#index"
      resources :posts, only: :index
      resource :profile, only: :show
      namespace :api do
        get "/status", to: "status#show"
      end
      defaults format: "xml" do
        get "/feed", to: "posts#index"
      end
      get "/after", to: "posts#index"
      get "/explicit", to: "posts#index", format: "xml"
    end
    get "/outer", to: "posts#index"
  end
  get "/outside", to: "posts#index"
"#,
    );
    let formats: Vec<_> = routes
        .iter()
        .map(|route| (route.path.as_str(), route.format.as_ref().map(|format| format.as_str())))
        .collect();
    assert_eq!(
        formats,
        vec![
            ("/", Some("json")),
            ("/posts", Some("json")),
            ("/profile", Some("json")),
            ("/api/status", Some("json")),
            ("/feed", Some("xml")),
            ("/after", Some("json")),
            ("/explicit", Some("json")),
            ("/outer", Some("html")),
            ("/outside", None),
        ]
    );
    assert_eq!(routes[3].controller.0.as_str(), "Api::StatusController");
}

#[test]
fn root_defaults_override_the_enclosing_format() {
    for enclosing in ["scope defaults: { format: :json }", "defaults format: :json"] {
        for root in ["root 'posts#index'", "root to: 'posts#index'"] {
            let routes = flat_routes(&format!(
                r#"
  {enclosing} do
    {root}, defaults: {{ format: :html }}
    get "/after", to: "posts#index"
  end
"#
            ));
            assert_eq!(routes.len(), 2);
            assert_eq!(routes[0].path, "/");
            assert_eq!(routes[0].as_name, "root");
            assert_eq!(routes[0].format.as_ref().unwrap().as_str(), "html");
            assert_eq!(routes[1].format.as_ref().unwrap().as_str(), "json");
        }
    }
}

#[test]
fn route_defaults_override_inherited_defaults_and_format_options() {
    for options in [
        "defaults: { format: :xml }",
        "format: 'html', defaults: { format: :xml }",
        "defaults: { format: :xml }, format: 'html'",
    ] {
        let routes = flat_routes(&format!(
            r#"
  defaults format: :json, user_id: "me" do
    get "/users/:user_id/profile", to: "profiles#show", {options}
    get "/after", to: "posts#index"
  end
"#
        ));
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].format.as_ref().unwrap().as_str(), "xml");
        assert_eq!(routes[0].param_defaults, vec![("user_id".into(), "me".into())]);
        assert_eq!(routes[1].format.as_ref().unwrap().as_str(), "json");
    }
}

#[test]
fn format_option_supplies_a_fallback_when_defaults_do_not_set_format() {
    let routes = flat_routes(
        r#"
  get "/plain", to: "posts#index", format: "xml"
  defaults user_id: "me" do
    get "/users/:user_id/posts", to: "posts#index", format: "xml"
  end
"#,
    );
    assert_eq!(routes.len(), 2);
    for route in routes {
        assert_eq!(route.format.as_ref().unwrap().as_str(), "xml");
    }
}

#[test]
fn resource_defaults_override_the_enclosing_format() {
    let routes = flat_routes(
        r#"
  defaults format: :json do
    resources :posts, only: :index, defaults: { format: :xml }
    resource :profile, only: :show, defaults: { format: :html }
    get "/after", to: "posts#index"
  end
"#,
    );
    let formats: Vec<_> = routes
        .iter()
        .map(|route| (route.path.as_str(), route.format.as_ref().unwrap().as_str()))
        .collect();
    assert_eq!(formats, vec![("/posts", "xml"), ("/profile", "html"), ("/after", "json")]);
}

#[test]
fn defaults_preserve_the_nearest_resource_parent() {
    for child in [
        "resources :comments, only: :show, defaults: { format: :json }",
        "defaults format: :json do\n  resources :comments, only: :show\nend",
    ] {
        let app = routes_app(&format!(
            r#"
  resources :accounts, only: :show do
    resources :posts, only: :show do
      {child}
    end
  end
"#
        ));
        let parent = roundhouse::lower::find_nested_parent(&app, "CommentsController")
            .expect("comments have a resource parent");
        assert_eq!(parent.plural, "posts");
        assert_eq!(parent.singular, "post");
        let routes = flatten_routes(&app);
        let comment = routes
            .iter()
            .find(|route| route.controller.0.as_str() == "CommentsController")
            .unwrap();
        assert_eq!(comment.path, "/accounts/:account_id/posts/:post_id/comments/:id");
        assert_eq!(comment.as_name, "account_post_comment");
    }
}

#[test]
fn route_local_defaults_preserve_member_and_collection_context() {
    let routes = flat_routes(
        r#"
  resources :posts, only: :show do
    member do
      get :export, defaults: { format: :json }
    end
    collection do
      get :search, defaults: { format: :json }
    end
    get :preview, on: :member, defaults: { format: :json }
  end
"#,
    );
    let defaulted: Vec<_> = routes
        .iter()
        .filter(|route| route.format.is_some())
        .map(|route| (route.path.as_str(), route.as_name.as_str()))
        .collect();
    assert_eq!(
        defaulted,
        vec![
            ("/posts/:id/export", "export_post"),
            ("/posts/search", "search_posts"),
            ("/posts/:id/preview", "preview_post"),
        ]
    );
}

#[test]
fn defaults_preserve_resource_controller_and_member_collection_context() {
    let routes = flat_routes(
        r#"
  resources :posts, only: :show do
    defaults format: :json do
      get :preview
    end
    member do
      defaults format: :json do
        get :export
      end
    end
    collection do
      defaults format: :json do
        get :search
      end
    end
  end
"#,
    );
    let defaulted: Vec<_> = routes
        .iter()
        .filter(|route| route.format.is_some())
        .map(|route| {
            assert_eq!(route.controller.0.as_str(), "PostsController");
            assert_eq!(route.format.as_ref().unwrap().as_str(), "json");
            (route.path.as_str(), route.as_name.as_str())
        })
        .collect();
    assert_eq!(
        defaulted,
        vec![
            ("/posts/:post_id/preview", "post_preview"),
            ("/posts/:id/export", "export_post"),
            ("/posts/search", "search_posts"),
        ]
    );
    assert!(routes.iter().any(|route| route.path == "/posts/:id" && route.format.is_none()));
}

#[test]
fn defaults_accept_explicit_hash_forms() {
    for declaration in [
        "defaults({ format: :json })",
        "defaults({ :format => \"json\" })",
    ] {
        let routes = flat_routes(&format!(
            "{declaration} do\n  get '/posts', to: 'posts#index'\nend"
        ));
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/posts");
        assert_eq!(routes[0].format.as_ref().unwrap().as_str(), "json");
    }
}

#[test]
fn string_default_keys_report_a_gap_instead_of_becoming_symbol_keys() {
    for declaration in [
        r#"defaults({ "user_id" => "me" }) do
  get '/users/:user_id', to: 'users#show'
end"#,
        "scope defaults: { \"user_id\" => \"me\" } do\nresources :users\nend",
        "get '/users/:user_id', to: 'users#show', defaults: { \"user_id\" => \"me\" }",
        r#"defaults({ format: :json, "format" => "xml" }) do
  defaults format: :html do
    resources :posts
  end
end"#,
    ] {
        let source = format!("Rails.application.routes.draw do\n{declaration}\nend");
        let error = ingest_routes(source.as_bytes(), "config/routes.rb")
            .expect_err("string keys must not be normalized to symbols");
        assert!(error.to_string().contains("route defaults"), "{error}");
    }
}

#[test]
fn defaults_block_merges_segment_defaults_with_an_outer_scope() {
    let params = helper_params(
        r#"
Rails.application.routes.draw do
  scope defaults: { user_id: "me" } do
    defaults format: :json do
      get "/users/:user_id/profile", to: "profiles#show", as: :profile
    end
  end
end
"#,
        "profile_path",
    );
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name.as_str(), "user_id");
    assert_eq!(params[0].kind, ParamKind::Optional);
}

#[test]
fn dynamic_defaults_report_a_gap_instead_of_silently_losing_the_format() {
    for declaration in [
        "defaults format: response_format",
        "defaults DEFAULTS",
        "defaults **options",
        "scope defaults: { format: response_format }",
    ] {
        let source = format!(
            "Rails.application.routes.draw do\n  {declaration} do\n    resources :posts\n  end\nend"
        );
        let error = ingest_routes(source.as_bytes(), "config/routes.rb").expect_err("dynamic defaults");
        assert!(error.to_string().contains("route defaults"), "{error}");
    }
}

#[test]
fn survey_preserves_defaults_routes_and_reports_unsupported_defaults() {
    roundhouse::ingest::survey::activate();
    let result = ingest_routes(
        br#"Rails.application.routes.draw do
  defaults format: :json do
    get "/posts", to: "posts#index"
  end
  defaults format: response_format do
    get "/dynamic", to: "posts#index"
  end
  defaults({ "user_id" => "me" }) do
    get "/users/:user_id", to: "users#show"
  end
  get "/outside", to: "posts#index"
end"#,
        "config/routes.rb",
    );
    let gaps = roundhouse::ingest::survey::drain();
    let mut app = roundhouse::App::default();
    app.routes = result.expect("survey ingest");
    let routes = flatten_routes(&app);
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    for gap in gaps {
        assert!(gap.to_string().contains("route defaults"), "{gap}");
    }
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].path, "/posts");
    assert_eq!(routes[0].format.as_ref().unwrap().as_str(), "json");
    assert_eq!(routes[1].path, "/outside");
    assert_eq!(routes[1].format, None);
}
