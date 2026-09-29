use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::App;
use roundhouse::expr::{Expr, ExprNode};
use roundhouse::ingest::{ingest_app_from_tree, ingest_routes};
use roundhouse::lower::routes::flatten_routes;

type Tree = HashMap<PathBuf, Vec<u8>>;

fn put(tree: &mut Tree, path: &str, source: &str) {
    tree.insert(path.into(), source.as_bytes().to_vec());
}

fn tree() -> Tree {
    let mut tree = Tree::new();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw do\n mount Blog::Engine, at: '/news', as: :journal\nend",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "module Blog; class Engine < Rails::Engine; isolate_namespace Blog; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw do\n resources :posts, only: [:index, :show]\nend",
    );
    tree
}

fn app(tree: Tree) -> App {
    ingest_app_from_tree(tree).expect("local engine ingest")
}

fn calls_helper(expr: &Expr, name: &str) -> bool {
    if matches!(&*expr.node, ExprNode::Send { recv, method, .. }
        if method.as_str() == name && recv.as_ref().is_none_or(|recv|
            matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "RouteHelpers")))
    {
        return true;
    }
    let mut found = false;
    expr.node
        .for_each_child(&mut |child| found |= calls_helper(child, name));
    found
}

fn call_type(expr: &Expr, name: &str) -> Option<roundhouse::ty::Ty> {
    if matches!(&*expr.node, ExprNode::Send { method, .. } if method.as_str() == name) {
        return expr.ty.clone();
    }
    let mut found = None;
    expr.node.for_each_child(&mut |child| {
        if found.is_none() {
            found = call_type(child, name);
        }
    });
    found
}

#[test]
fn isolated_engine_uses_declared_namespace_and_mount_name() {
    let app = app(tree());
    let routes = flatten_routes(&app);
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].path, "/news/posts");
    assert_eq!(routes[0].controller.0.as_str(), "Blog::PostsController");
    assert_eq!(routes[0].as_name, "journal_posts");
    assert_eq!(routes[1].path_params, ["id"]);
}

#[test]
fn default_proxy_name_comes_from_engine_not_directory() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/news' }",
    );
    assert_eq!(flatten_routes(&app(tree))[0].as_name, "blog_posts");
}

#[test]
fn host_namespace_does_not_qualify_engine_controllers() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { namespace :admin do; mount Blog::Engine, at: '/news'; end }",
    );
    let routes = flatten_routes(&app(tree));
    assert_eq!(routes[0].path, "/admin/news/posts");
    assert_eq!(routes[0].controller.0.as_str(), "Blog::PostsController");
    assert_eq!(routes[0].as_name, "admin_blog_posts");
}

#[test]
fn nonisolated_engine_keeps_global_controllers() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "class Blog::Engine < Rails::Engine; engine_name 'publishing'; end",
    );
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/news' }",
    );
    let routes = flatten_routes(&app(tree));
    assert_eq!(routes[0].controller.0.as_str(), "PostsController");
    assert_eq!(routes[0].as_name, "publishing_posts");
}

#[test]
fn split_routes_keep_engine_draws_separate_from_host() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { draw :shared }",
    );
    put(
        &mut tree,
        "config/routes/shared.rb",
        "draw :empty\nmount Blog::Engine, at: '/news'",
    );
    put(&mut tree, "config/routes/empty.rb", "");
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw { draw 'api/shared' }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes/api/shared.rb",
        "resources :posts, only: [:index]",
    );
    assert_eq!(flatten_routes(&app(tree))[0].path, "/news/posts");
}

#[test]
fn route_defaults_survive_engine_draws_and_resource_nesting() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw do\n resources :accounts, only: :show do\n mount Blog::Engine, at: '/news', as: :journal\n end\nend",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "class Blog::Engine < Rails::Engine; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        r#"Blog::Engine.routes.draw do
  defaults format: :json do
    resources :posts, only: :show do
      member do
        get :preview, defaults: { format: :xml }
      end
      defaults format: :xml do
        draw :comments
      end
    end
  end
  get '/status', to: 'status#show'
end"#,
    );
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes/comments.rb",
        "resources :comments, only: :show, defaults: { format: :html }",
    );
    let app = app(tree);
    let routes = flatten_routes(&app);
    let mounted: Vec<_> = routes
        .iter()
        .filter(|route| route.path.contains("/news/"))
        .map(|route| {
            (
                route.path.as_str(),
                route.as_name.as_str(),
                route.format.as_ref().map(|format| format.as_str()),
            )
        })
        .collect();
    assert_eq!(mounted, vec![
        ("/accounts/:account_id/news/posts/:id", "account_journal_post", Some("json")),
        ("/accounts/:account_id/news/posts/:id/preview", "account_journal_preview_post", Some("xml")),
        ("/accounts/:account_id/news/posts/:post_id/comments/:id", "account_journal_post_comment", Some("html")),
        ("/accounts/:account_id/news/status", "account_journal_status", None),
    ]);
    let parent = roundhouse::lower::find_nested_parent(&app, "CommentsController").unwrap();
    assert_eq!(parent.plural, "posts");
    assert!(roundhouse::lower::find_nested_parent(&app, "PostsController").is_none());
}

#[test]
fn recursion_is_an_error_and_does_not_leak_state_to_next_ingest() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw { mount Blog::Engine, at: '/again' }",
    );
    let error = ingest_app_from_tree(tree).unwrap_err().to_string();
    assert!(
        error.contains("recursive route draw or engine mount"),
        "{error}"
    );
    assert!(
        ingest_routes(b"Rails.application.routes.draw {}", "routes.rb")
            .unwrap()
            .redirects
            .is_empty()
    );
}

#[test]
fn engine_sources_preserve_paths_and_host_views_override() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; posts_path; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<p>engine</p>",
    );
    put(
        &mut tree,
        "app/views/blog/posts/index.html.erb",
        "<p>host</p>",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/show.html.haml",
        "%p Engine HAML",
    );
    let app = app(tree);
    assert!(
        app.controllers
            .iter()
            .any(|c| c.name.0.as_str() == "Blog::PostsController")
    );
    assert!(app.sources.iter().any(|s| s.path == "engines/not_the_namespace/app/controllers/blog/posts_controller.rb"));
    assert!(
        app.sources
            .iter()
            .any(|s| s.path.ends_with("show.html.haml"))
    );
    assert!(
        app.sources
            .iter()
            .any(|s| s.path == "app/views/blog/posts/index.html.erb" && s.text.contains("host"))
    );
    assert!(!app.sources.iter().any(|s| s.text.contains("<p>engine</p>")));
    assert!(
        app.controllers
            .iter()
            .flat_map(|controller| controller.actions())
            .any(|action| calls_helper(&action.body, "journal_posts_path"))
    );
}

#[test]
fn engine_models_use_isolated_table_prefix() {
    let mut tree = tree();
    put(
        &mut tree,
        "db/schema.rb",
        "ActiveRecord::Schema.define(version: 1) do; create_table :blog_posts do |t|; t.string :title; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/models/blog/post.rb",
        "class Blog::Post < ApplicationRecord; end",
    );
    let app = app(tree);
    assert_eq!(app.models.len(), 1);
    assert_eq!(app.models[0].table.0.as_str(), "blog_posts");
}

#[test]
fn engine_models_use_the_hosts_configured_postgresql_schema() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/application.rb",
        "config.active_record.schema_format = :sql",
    );
    put(
        &mut tree,
        "db/schema.rb",
        "ActiveRecord::Schema.define do; create_table :stale; end",
    );
    put(
        &mut tree,
        "db/structure.sql",
        "CREATE TABLE public.blog_posts (id bigint PRIMARY KEY, title text NOT NULL);",
    );
    let model_path = "engines/not_the_namespace/app/models/blog/post.rb";
    put(
        &mut tree,
        model_path,
        "class Blog::Post < ApplicationRecord; end",
    );

    let app = app(tree);
    assert_eq!(app.models.len(), 1);
    let model = &app.models[0];
    assert_eq!(model.table.0.as_str(), "blog_posts");
    assert_eq!(
        model.attributes.fields[&roundhouse::Symbol::from("title")],
        roundhouse::ty::Ty::Str,
    );
    assert_eq!(app.schema.tables.len(), 1);
    assert!(app.schema.postgresql.is_some());
    assert!(app.sources.iter().any(|source| source.path == model_path));
}

#[test]
fn host_proxy_calls_resolve_to_compiled_helpers() {
    let mut tree = tree();
    put(
        &mut tree,
        "app/controllers/home_controller.rb",
        "class HomeController < ApplicationController; def index; journal.posts_path; end; end",
    );
    let app = app(tree);
    assert!(
        app.controllers
            .iter()
            .flat_map(|controller| controller.actions())
            .any(|action| calls_helper(&action.body, "journal_posts_path"))
    );
}

#[test]
fn redirects_from_failed_ingest_do_not_leak() {
    assert!(
        ingest_routes(
            b"Rails.application.routes.draw { get '/old', to: redirect('/new'); unknown_dsl }",
            "broken.rb"
        )
        .is_err()
    );
    let next = ingest_routes(b"Rails.application.routes.draw {}", "next.rb").unwrap();
    assert!(next.redirects.is_empty());
}

#[test]
fn direct_helpers_in_split_files_are_collected() {
    let draws = HashMap::from([(
        "links".into(),
        (b"direct(:home) { '/home' }".to_vec(), "links.rb".into()),
    )]);
    let table = roundhouse::ingest::routes::ingest_routes_with_draws(
        b"Rails.application.routes.draw { draw :links }",
        "routes.rb",
        &draws,
    )
    .unwrap();
    assert_eq!(table.direct_helpers[0].name.as_str(), "home");
}

#[test]
fn hashrocket_mount_and_custom_engine_class_are_supported() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "module Blog; class Railtie < Rails::Engine; isolate_namespace Blog; end; end",
    );
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Railtie => '/news' }",
    );
    assert_eq!(flatten_routes(&app(tree))[0].as_name, "blog_posts");
}

#[test]
fn unused_split_file_does_not_load_engine_sources() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw {}",
    );
    put(
        &mut tree,
        "config/routes/unused.rb",
        "mount Blog::Engine, at: '/unused'",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; unknown_call; end; end",
    );
    let app = app(tree);
    assert!(app.controllers.is_empty());
    assert!(app.routes.entries.is_empty());
}

#[test]
fn nested_local_mounts_are_discovered_transitively() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw { mount Search::Engine, at: '/search' }",
    );
    put(
        &mut tree,
        "components/search/lib/search/engine.rb",
        "module Search; class Engine < Rails::Engine; isolate_namespace Search; end; end",
    );
    put(
        &mut tree,
        "components/search/config/routes.rb",
        "Search::Engine.routes.draw { root 'results#index' }",
    );
    let routes = flatten_routes(&app(tree));
    assert_eq!(routes[0].path, "/news/search");
    assert_eq!(routes[0].controller.0.as_str(), "Search::ResultsController");
    assert_eq!(routes[0].as_name, "journal_search_root");
}

#[test]
fn engine_name_after_isolation_does_not_change_model_prefix() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "module Blog; class Engine < Rails::Engine; isolate_namespace Blog; engine_name 'publishing'; end; end",
    );
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/news' }",
    );
    put(
        &mut tree,
        "db/schema.rb",
        "ActiveRecord::Schema.define(version: 1) do; create_table :blog_posts do |t|; t.string :title; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/models/blog/post.rb",
        "class Blog::Post < ApplicationRecord; end",
    );
    let app = app(tree);
    assert_eq!(flatten_routes(&app)[0].as_name, "publishing_posts");
    assert_eq!(app.models[0].table.0.as_str(), "blog_posts");
}

#[test]
fn recursive_split_draw_is_reported() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw { draw :loop }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes/loop.rb",
        "draw :loop",
    );
    let error = ingest_app_from_tree(tree).unwrap_err().to_string();
    assert!(error.contains("recursive route draw"), "{error}");
}

#[test]
fn same_engine_mounted_twice_ingests_sources_once() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/one', as: :one; mount Blog::Engine, at: '/two', as: :two }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; 'ok'; end; end",
    );
    let app = app(tree);
    assert_eq!(app.controllers.len(), 1);
    let routes = flatten_routes(&app);
    assert_eq!(routes.len(), 4);
    assert_eq!(routes[2].path, "/two/posts");
}

#[test]
fn unsupported_engine_direct_helpers_are_explicit() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw { direct(:article) { '/posts' } }",
    );
    let error = ingest_app_from_tree(tree).unwrap_err().to_string();
    assert!(
        error.contains("direct helpers inside local engines"),
        "{error}"
    );
    assert!(
        error.contains("engines/not_the_namespace/config/routes.rb"),
        "{error}"
    );
}

#[test]
fn unsupported_mount_options_are_not_silently_ignored() {
    for options in [
        "at: '/blog', via: :get",
        "at: '/blog', as: proxy_name",
        "at: mount_path",
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "config/routes.rb",
            &format!("Rails.application.routes.draw {{ mount Blog::Engine, {options} }}"),
        );
        let error = ingest_app_from_tree(tree).unwrap_err().to_string();
        assert!(error.contains("literal `at:` and `as:`"), "{error}");
    }
}

#[test]
fn overridden_engine_view_keeps_its_route_context() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "engine",
    );
    put(
        &mut tree,
        "app/views/blog/posts/index.html.erb",
        "<%= posts_path %>",
    );
    let app = app(tree);
    assert!(
        app.views
            .iter()
            .any(|view| calls_helper(&view.body, "journal_posts_path"))
    );
    assert!(
        app.sources
            .iter()
            .any(|s| s.path == "app/views/blog/posts/index.html.erb")
    );
}

#[test]
fn on_disk_ingest_handles_absent_optional_directories() {
    let root = std::env::temp_dir().join(format!(
        "roundhouse-engine-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for (path, bytes) in tree() {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let result = roundhouse::ingest::ingest_app(&root);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(flatten_routes(&result.unwrap())[0].path, "/news/posts");
}

#[test]
fn local_engine_ir_serializes_without_losing_mount_boundaries() {
    let app = app(tree());
    let serialized = serde_json::to_string(&app).unwrap();
    let restored: App = serde_json::from_str(&serialized).unwrap();
    assert_eq!(app, restored);
}

#[test]
fn engine_route_helpers_analyze_without_errors() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; posts_path; end; def show; post_path(1); end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<%= posts_path %>",
    );
    let mut app = app(tree);
    let mut analyzer = roundhouse::analyze::Analyzer::new(&app);
    analyzer.analyze(&mut app);
    roundhouse::lower::apply_post_analyze_lowerings(&mut app, analyzer.class_registry());
    let errors: Vec<_> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == roundhouse::analyze::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn conflicting_engine_files_are_an_error() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/blog'; mount Search::Engine, at: '/search' }",
    );
    put(
        &mut tree,
        "packs/search/lib/search/engine.rb",
        "module Search; class Engine < Rails::Engine; isolate_namespace Search; end; end",
    );
    put(
        &mut tree,
        "packs/search/config/routes.rb",
        "Search::Engine.routes.draw {}",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/services/common.rb",
        "class Common; end",
    );
    put(
        &mut tree,
        "packs/search/app/services/common.rb",
        "class Common; end",
    );
    let error = ingest_app_from_tree(tree).unwrap_err().to_string();
    assert!(error.contains("local engine source collision"), "{error}");
}

#[test]
fn malformed_engine_declaration_keeps_original_error_path() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "module Blog; class Engine < Rails::Engine; isolate_namespace Blog; end",
    );
    let error = ingest_app_from_tree(tree).unwrap_err().to_string();
    assert!(
        error.contains("parse error in engines/not_the_namespace/lib/blog/engine.rb"),
        "{error}"
    );
}

fn generated_helper(app: &App, name: &str) -> roundhouse::dialect::LibraryFunction {
    roundhouse::lower::lower_routes_to_library_functions(app)
        .into_iter()
        .find(|helper| helper.name.as_str() == name)
        .unwrap_or_else(|| panic!("missing helper {name}"))
}

fn assert_literal_helper(app: &App, name: &str, expected: &str) {
    let helper = generated_helper(app, name);
    assert_eq!(
        *helper.body.node,
        ExprNode::Lit {
            value: roundhouse::expr::Literal::Str {
                value: expected.into()
            }
        }
    );
}

fn emitted_controllers(mut app: App) -> String {
    let mut analyzer = roundhouse::analyze::Analyzer::new(&app);
    analyzer.analyze(&mut app);
    roundhouse::lower::apply_post_analyze_lowerings(&mut app, analyzer.class_registry());
    roundhouse::emit::ruby::emit_lowered_controllers(&app)
        .into_iter()
        .map(|file| file.content)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn optional_mount_paths_are_reported_before_flattening() {
    for routes in [
        "mount Blog::Engine, at: '(/:locale)/news'",
        "mount Blog::Engine => '/news(/:locale)'",
        "scope '(:locale)' do; mount Blog::Engine, at: '/news'; end",
        "scope path: '(/:locale)' do; resources :accounts do; mount Blog::Engine, at: '/news'; end; end",
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "config/routes.rb",
            &format!("Rails.application.routes.draw {{ {routes} }}"),
        );
        let error = ingest_app_from_tree(tree).unwrap_err().to_string();
        assert!(
            error.contains("optional path segments in local engine mount"),
            "{error}"
        );
        assert!(error.contains("config/routes.rb"), "{error}");
    }
}

#[test]
fn optional_mount_scope_survives_draws_and_does_not_leak_to_siblings() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { scope '(:locale)' do; draw :mounts; end; mount Blog::Engine, at: '/news', as: :journal }",
    );
    put(
        &mut tree,
        "config/routes/mounts.rb",
        "mount Blog::Engine, at: '/bad'",
    );
    roundhouse::ingest::survey::activate();
    let result = ingest_app_from_tree(tree);
    let gaps = roundhouse::ingest::survey::drain();
    let app = result.unwrap();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].to_string().contains("config/routes/mounts.rb"));
    assert!(gaps[0].to_string().contains("optional path segments"));
    let routes = flatten_routes(&app);
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].path, "/news/posts");
    assert_literal_helper(&app, "journal_path", "/news");
}

#[test]
fn main_app_helpers_bypass_engine_method_shadows() {
    for definition in [
        "class Blog::BaseController < ApplicationController; def posts_path; '/custom'; end; end",
        "class Blog::BaseController < ApplicationController; include Blog::Links; end",
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "config/routes.rb",
            "Rails.application.routes.draw { resources :posts, only: :index; mount Blog::Engine, at: '/news', as: :journal }",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/base_controller.rb",
            definition,
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/concerns/blog_links.rb",
            "module Blog::Links; def posts_path; '/custom'; end; end",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
            "class Blog::PostsController < Blog::BaseController; def index; redirect_to main_app.posts_path; end; end",
        );
        let output = emitted_controllers(app(tree));
        assert!(output.contains("RouteHelpers.posts_path"), "{output}");
        assert!(!output.contains("self.posts_path"), "{output}");
        assert!(!output.contains("main_app"), "{output}");
    }
}

#[test]
fn main_app_helpers_keep_url_options_and_call_site_types() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { resources :posts, only: :show; mount Blog::Engine, at: '/news', as: :journal }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; redirect_to main_app.post_url('slug', title: :small, format: :json, host: 'example.test', protocol: 'https'); end; def post_url; '/custom'; end; end",
    );
    let mut app = app(tree);
    let mut analyzer = roundhouse::analyze::Analyzer::new(&app);
    analyzer.analyze(&mut app);
    roundhouse::lower::apply_post_analyze_lowerings(&mut app, analyzer.class_registry());
    let errors: Vec<_> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == roundhouse::analyze::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let helper = generated_helper(&app, "post_json_path");
    let roundhouse::ty::Ty::Fn { params, .. } = helper.signature.unwrap() else {
        panic!()
    };
    assert_eq!(params[0].ty, roundhouse::ty::Ty::Str);
    assert!(helper.params.iter().any(|p| p.name.as_str() == "title"));
    assert!(!helper.params.iter().any(|p| p.name.as_str() == "host"));
    let output = roundhouse::emit::ruby::emit_lowered_controllers(&app)
        .into_iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(output.contains("RouteHelpers.post_json_path"), "{output}");
    assert!(
        output.contains("example.test") && output.contains("https"),
        "{output}"
    );
    assert!(!output.contains("self.post_url"), "{output}");
}

#[test]
fn controller_methods_shadow_engine_proxies() {
    for definition in [
        "class Blog::BaseController < ApplicationController; def journal; Blog::Link.new; end; end",
        "class Blog::BaseController < ApplicationController; include Blog::Links; end",
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/base_controller.rb",
            definition,
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/concerns/blog_links.rb",
            "module Blog::Links; def journal; Blog::Link.new; end; end",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/services/blog/link.rb",
            "class Blog::Link; def posts_path; '/custom'; end; end",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
            "class Blog::PostsController < Blog::BaseController; def index; redirect_to journal.posts_path; end; end",
        );
        let output = emitted_controllers(app(tree));
        assert!(
            !output.contains("RouteHelpers.journal_posts_path"),
            "{output}"
        );
        assert!(output.contains("journal.posts_path"), "{output}");
    }
}

#[test]
fn view_helpers_shadow_engine_proxies_but_not_main_app_helpers() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { resources :posts, only: :index; mount Blog::Engine, at: '/news', as: :journal }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/helpers/blog/posts_helper.rb",
        "module Blog::PostsHelper; def journal; Blog::Link.new; end; def posts_path; '/custom'; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/services/blog/link.rb",
        "class Blog::Link; def posts_path; '/custom'; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<%= journal.posts_path %><%= main_app.posts_path %>",
    );
    let mut app = app(tree);
    roundhouse::session::analyze_and_lower(&mut app);
    let output = roundhouse::emit::ruby::emit_lowered_views(&app)
        .into_iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(output.contains("RouteHelpers.posts_path"), "{output}");
    assert!(
        !output.contains("RouteHelpers.journal_posts_path"),
        "{output}"
    );
}

#[test]
fn mount_paths_join_with_one_slash_in_routes_and_helpers() {
    for (mount, expected) in [("/", "/posts"), ("/news/", "/news/posts")] {
        let mut tree = tree();
        put(
            &mut tree,
            "config/routes.rb",
            &format!(
                "Rails.application.routes.draw {{ mount Blog::Engine, at: '{mount}', as: :journal }}"
            ),
        );
        let app = app(tree);
        assert_eq!(flatten_routes(&app)[0].path, expected);
        assert_literal_helper(&app, "journal_posts_path", expected);
    }
}

#[test]
fn mount_point_helpers_exist_even_without_engine_routes() {
    for (mount, expected) in [("/", "/"), ("/news/", "/news")] {
        let mut tree = tree();
        put(
            &mut tree,
            "config/routes.rb",
            &format!(
                "Rails.application.routes.draw {{ mount Blog::Engine, at: '{mount}', as: :journal }}"
            ),
        );
        put(
            &mut tree,
            "engines/not_the_namespace/config/routes.rb",
            "Blog::Engine.routes.draw {}",
        );
        let app = app(tree);
        assert!(flatten_routes(&app).is_empty());
        assert_literal_helper(&app, "journal_path", expected);
    }
}

#[test]
fn mount_point_helpers_keep_scoped_parameters_and_defaults() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { scope ':locale', defaults: { locale: 'en' } do; mount Blog::Engine, at: '/news', as: :journal; end }",
    );
    let app = app(tree);
    let helper = generated_helper(&app, "journal_path");
    assert_eq!(helper.params.len(), 1);
    assert_eq!(helper.params[0].name.as_str(), "locale");
    assert!(matches!(&*helper.params[0].default.as_ref().unwrap().node,
        ExprNode::Lit { value: roundhouse::expr::Literal::Str { value } } if value == "en"));
    assert_eq!(flatten_routes(&app).len(), 2);
    assert!(roundhouse::lower::routes::helper_id_segments(&app).contains_key("journal_path"));
}

#[test]
fn unrelated_methods_do_not_shadow_engine_routes() {
    for (path, source) in [
        (
            "app/models/link.rb",
            "class Link < ApplicationRecord; def posts_path; '/other'; end; end",
        ),
        (
            "app/services/link.rb",
            "class Link; def posts_path; '/other'; end; end",
        ),
        (
            "app/controllers/links_controller.rb",
            "class LinksController < ApplicationController; def posts_path; '/other'; end; end",
        ),
    ] {
        let mut tree = tree();
        put(&mut tree, path, source);
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
            "class Blog::PostsController < ApplicationController; def index; posts_path; end; end",
        );
        let output = emitted_controllers(app(tree));
        assert!(
            output.contains("RouteHelpers.journal_posts_path"),
            "{path}: {output}"
        );
        assert!(
            !output.contains("RouteHelpers.posts_path"),
            "{path}: {output}"
        );
    }
}

#[test]
fn inherited_and_included_methods_still_shadow_engine_routes() {
    for definition in [
        "class Blog::BaseController < ApplicationController; def posts_path; '/custom'; end; end",
        "class Blog::BaseController < ApplicationController; include Blog::Links; end",
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/base_controller.rb",
            definition,
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/concerns/blog_links.rb",
            "module Blog::Links; def posts_path; '/custom'; end; end",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
            "class Blog::PostsController < Blog::BaseController; def index; posts_path; end; end",
        );
        let app = app(tree);
        let controller = app
            .controllers
            .iter()
            .find(|c| c.name.0.as_str() == "Blog::PostsController")
            .unwrap();
        assert!(
            calls_helper(&controller.actions().next().unwrap().body, "posts_path"),
            "{definition}"
        );
        assert!(!emitted_controllers(app).contains("RouteHelpers.journal_posts_path"));
    }
}

#[test]
fn host_controller_overrides_retain_isolated_routes_and_main_app() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { resources :posts, only: [:index]; mount Blog::Engine, at: '/news', as: :journal }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; 'engine'; end; end",
    );
    put(
        &mut tree,
        "app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; posts_path; end; def host; main_app.posts_path; end; end",
    );
    let output = emitted_controllers(app(tree));
    assert!(
        output.contains("RouteHelpers.journal_posts_path"),
        "{output}"
    );
    assert!(output.contains("RouteHelpers.posts_path"), "{output}");
    assert!(!output.contains("main_app"), "{output}");
}

fn with_nested_engine(mut tree: Tree) -> Tree {
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        "Blog::Engine.routes.draw { mount Search::Engine, at: '/search' }",
    );
    put(
        &mut tree,
        "components/search/lib/search/engine.rb",
        "module Search; class Engine < Rails::Engine; isolate_namespace Search; end; end",
    );
    put(
        &mut tree,
        "components/search/config/routes.rb",
        "Search::Engine.routes.draw { root 'results#index' }",
    );
    tree
}

#[test]
fn nested_engine_proxies_resolve_and_mount_point_helpers_are_qualified() {
    let mut tree = with_nested_engine(tree());
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; search.root_path; end; def search_home; search_path; end; end",
    );
    put(
        &mut tree,
        "app/controllers/home_controller.rb",
        "class HomeController < ApplicationController; def index; search.root_path; end; end",
    );
    let app = app(tree);
    assert_literal_helper(&app, "journal_search_path", "/news/search");
    assert_literal_helper(&app, "journal_search_root_path", "/news/search");
    let output = emitted_controllers(app);
    assert!(
        output.contains("RouteHelpers.journal_search_root_path"),
        "{output}"
    );
    assert!(
        output.contains("RouteHelpers.journal_search_path"),
        "{output}"
    );
    assert!(!output.contains("search.root_path"), "{output}");
}

#[test]
fn direct_helpers_resolve_engine_proxies_to_generated_functions() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/news', as: :journal; direct(:latest) { journal.posts_path } }",
    );
    let app = app(tree);
    let helper = generated_helper(&app, "latest_path");
    let ExprNode::Send {
        recv: Some(recv),
        method,
        ..
    } = &*helper.body.node
    else {
        panic!("unqualified helper: {:?}", helper.body);
    };
    assert_eq!(method.as_str(), "journal_posts_path");
    assert!(
        matches!(&*recv.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "RouteHelpers")
    );
    assert_literal_helper(&app, "journal_posts_path", "/news/posts");
}

#[test]
fn unused_engine_declaration_errors_are_deferred_until_mounted() {
    for declaration in [
        "module Unused; class Engine < Rails::Engine; engine_name ENV.fetch('ENGINE_NAME'); end; end",
        "module Unused; class Engine < Rails::Engine; isolate_namespace Unused; end",
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "engines/unused/lib/unused/engine.rb",
            declaration,
        );
        put(
            &mut tree,
            "engines/unused/config/routes.rb",
            "Unused::Engine.routes.draw {}",
        );
        assert!(ingest_app_from_tree(tree.clone()).is_ok());
        put(
            &mut tree,
            "engines/not_the_namespace/config/routes.rb",
            "Blog::Engine.routes.draw { mount Unused::Engine, at: '/unused' }",
        );
        let error = ingest_app_from_tree(tree).unwrap_err().to_string();
        assert!(
            error.contains("engines/unused/lib/unused/engine.rb"),
            "{error}"
        );
    }
}

#[test]
fn duplicate_unused_engine_declarations_do_not_abort_host_ingest() {
    let mut tree = tree();
    for dir in ["engines/unused", "components/unused"] {
        put(
            &mut tree,
            &format!("{dir}/lib/unused/engine.rb"),
            "module Unused; class Engine < Rails::Engine; isolate_namespace Unused; end; end",
        );
        put(
            &mut tree,
            &format!("{dir}/config/routes.rb"),
            "Unused::Engine.routes.draw {}",
        );
    }
    assert!(ingest_app_from_tree(tree.clone()).is_ok());
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Unused::Engine, at: '/unused' }",
    );
    assert!(
        ingest_app_from_tree(tree)
            .unwrap_err()
            .to_string()
            .contains("multiple local engines declare `Unused::Engine`")
    );
}

fn emitted_views(mut app: App) -> Vec<roundhouse::emit::EmittedFile> {
    roundhouse::session::analyze_and_lower(&mut app);
    roundhouse::emit::ruby::emit_lowered_views(&app)
}

#[test]
fn engine_proxy_bypasses_a_controller_method_with_the_generated_name() {
    let mut tree = tree();
    put(
        &mut tree,
        "app/controllers/home_controller.rb",
        "class HomeController < ApplicationController; def index; journal.posts_path; end; def journal_posts_path; '/wrong'; end; end",
    );
    let output = emitted_controllers(app(tree));
    assert!(
        output.contains("RouteHelpers.journal_posts_path"),
        "{output}"
    );
    assert!(!output.contains("self.journal_posts_path"), "{output}");
}

#[test]
fn isolated_engine_helpers_do_not_replace_host_route_helpers() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { resources :posts, only: :index; mount Blog::Engine, at: '/news', as: :journal }",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/helpers/blog/posts_helper.rb",
        "module Blog::PostsHelper; def posts_path; '/engine-only'; end; end",
    );
    put(
        &mut tree,
        "app/views/home/index.html.erb",
        "<%= posts_path %>",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<%= posts_path %>",
    );
    let output = emitted_views(app(tree));
    let host = output
        .iter()
        .find(|file| file.path.ends_with("home/index.rb"))
        .unwrap();
    let engine = output
        .iter()
        .find(|file| file.path.ends_with("blog/posts/index.rb"))
        .unwrap();
    assert!(
        host.content.contains("RouteHelpers.posts_path"),
        "{}",
        host.content
    );
    assert!(
        !host.content.contains("Blog::PostsHelper.posts_path"),
        "{}",
        host.content
    );
    assert!(
        engine.content.contains("Blog::PostsHelper.posts_path"),
        "{}",
        engine.content
    );
}

#[test]
fn isolated_helpers_keep_distinct_return_and_parameter_types() {
    let mut tree = tree();
    put(
        &mut tree,
        "app/helpers/application_helper.rb",
        "module ApplicationHelper; def caption(value); value + 1; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/helpers/blog/posts_helper.rb",
        "module Blog::PostsHelper; def caption(value); value + '!'; end; end",
    );
    put(
        &mut tree,
        "app/views/home/index.html.erb",
        "<%= caption(1) %>",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<%= caption('engine') %>",
    );
    let mut app = app(tree);
    app = serde_json::from_str(&serde_json::to_string(&app).unwrap()).unwrap();
    let mut analyzer = roundhouse::analyze::Analyzer::new(&app);
    analyzer.analyze(&mut app);
    for (class, expected) in [
        ("ApplicationHelper", roundhouse::ty::Ty::Int),
        ("Blog::PostsHelper", roundhouse::ty::Ty::Str),
    ] {
        let params = app
            .inferred_method_params
            .get(&(roundhouse::ClassId(class.into()), "caption".into()))
            .unwrap();
        assert_eq!(params[0], expected, "{class}");
    }
    let errors: Vec<_> = roundhouse::analyze::diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == roundhouse::analyze::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let output = emitted_views(app);
    let host = output
        .iter()
        .find(|file| file.path.ends_with("home/index.rb"))
        .unwrap();
    let engine = output
        .iter()
        .find(|file| file.path.ends_with("blog/posts/index.rb"))
        .unwrap();
    assert!(
        host.content.contains("ApplicationHelper.caption(1)"),
        "{}",
        host.content
    );
    assert!(
        engine
            .content
            .contains("Blog::PostsHelper.caption(\"engine\")"),
        "{}",
        engine.content
    );
}

#[test]
fn main_app_url_helpers_in_views_lower_to_absolute_urls() {
    for (call, expected) in [
        ("main_app.posts_url", "Rails.application.domain"),
        (
            "main_app.posts_url(host: 'example.test', protocol: 'https')",
            "example.test",
        ),
        (
            "main_app.posts_url(only_path: true)",
            "RouteHelpers.posts_path",
        ),
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "config/routes.rb",
            "Rails.application.routes.draw { resources :posts, only: :index; mount Blog::Engine, at: '/news', as: :journal }",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
            &format!("<%= link_to 'Posts', {call} %>"),
        );
        let output = emitted_views(app(tree))
            .into_iter()
            .map(|file| file.content)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains(expected), "{call}: {output}");
        assert!(
            output.contains("RouteHelpers.posts_path"),
            "{call}: {output}"
        );
        assert!(
            !output.contains("RouteHelpers.posts_url"),
            "{call}: {output}"
        );
        if call.contains("only_path") {
            assert!(!output.contains("Rails.application.domain"), "{output}");
        }
        if call == "main_app.posts_url" {
            assert!(output.contains("Rails.application.protocol"), "{output}");
        }
    }
}

#[test]
fn direct_engine_url_helpers_keep_format_and_query_options() {
    let mut tree = tree();
    put(
        &mut tree,
        "config/routes.rb",
        "Rails.application.routes.draw { mount Blog::Engine, at: '/news', as: :journal; direct(:latest) { journal.posts_url(format: :json, page: 'two') } }",
    );
    let mut app = app(tree);
    roundhouse::session::analyze_and_lower(&mut app);
    let helper = generated_helper(&app, "latest_path");
    assert!(
        calls_helper(&helper.body, "journal_posts_json_path"),
        "{:?}",
        helper.body
    );
    assert!(
        generated_helper(&app, "journal_posts_json_path")
            .params
            .iter()
            .any(|p| p.name.as_str() == "page")
    );
}

#[test]
fn nonisolated_engine_helpers_remain_available_to_host_views() {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/lib/blog/engine.rb",
        "class Blog::Engine < Rails::Engine; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/helpers/blog/posts_helper.rb",
        "module Blog::PostsHelper; def caption; 'shared'; end; end",
    );
    put(&mut tree, "app/views/home/index.html.erb", "<%= caption %>");
    let output = emitted_views(app(tree))
        .into_iter()
        .map(|file| file.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(output.contains("Blog::PostsHelper.caption"), "{output}");
}

#[test]
fn isolated_helper_keywords_use_their_own_defaults() {
    let mut tree = tree();
    put(
        &mut tree,
        "app/helpers/application_helper.rb",
        "module ApplicationHelper; def caption(prefix: 'host', suffix: '!'); prefix + suffix; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/helpers/blog/posts_helper.rb",
        "module Blog::PostsHelper; def caption(prefix: 'engine', suffix: '!'); prefix + suffix; end; end",
    );
    put(
        &mut tree,
        "app/views/home/index.html.erb",
        "<%= caption(suffix: '?') %>",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<%= caption(suffix: '?') %>",
    );
    let output = emitted_views(app(tree));
    let host = output
        .iter()
        .find(|file| file.path.ends_with("home/index.rb"))
        .unwrap();
    let engine = output
        .iter()
        .find(|file| file.path.ends_with("blog/posts/index.rb"))
        .unwrap();
    assert!(
        host.content
            .contains("ApplicationHelper.caption(\"host\", \"?\")"),
        "{}",
        host.content
    );
    assert!(
        engine
            .content
            .contains("Blog::PostsHelper.caption(\"engine\", \"?\")"),
        "{}",
        engine.content
    );
}

fn with_nested_namespace(mounts: &str) -> Tree {
    let mut tree = tree();
    put(
        &mut tree,
        "engines/not_the_namespace/config/routes.rb",
        &format!("Blog::Engine.routes.draw {{ resources :posts, only: :index; {mounts} }}"),
    );
    put(
        &mut tree,
        "components/search/lib/blog/search/engine.rb",
        "module Blog; module Search; class Engine < Rails::Engine; isolate_namespace Blog::Search; end; end; end",
    );
    put(
        &mut tree,
        "components/search/config/routes.rb",
        "Blog::Search::Engine.routes.draw { resources :posts, only: :index }",
    );
    put(
        &mut tree,
        "app/controllers/blog/search/posts_controller.rb",
        "class Blog::Search::PostsController < ApplicationController; def index; posts_path; end; end",
    );
    put(
        &mut tree,
        "app/views/blog/search/posts/index.html.erb",
        "<%= posts_path %>",
    );
    tree
}

#[test]
fn nested_namespace_overrides_use_the_most_specific_engine() {
    let app = app(with_nested_namespace(
        "mount Blog::Search::Engine, at: '/search'",
    ));
    let name = "journal_blog_search_posts_path";
    assert_literal_helper(&app, name, "/news/search/posts");
    assert!(calls_helper(
        &app.controllers[0].actions().next().unwrap().body,
        name
    ));
    assert!(calls_helper(&app.views[0].body, name));
    assert!(emitted_controllers(app.clone()).contains(&format!("RouteHelpers.{name}")));
    assert!(
        emitted_views(app)[0]
            .content
            .contains(&format!("RouteHelpers.{name}"))
    );
}

#[test]
fn repeated_nested_mounts_do_not_fall_back_to_the_parent_engine() {
    let app = app(with_nested_namespace(
        "mount Blog::Search::Engine, at: '/one', as: :one; mount Blog::Search::Engine, at: '/two', as: :two",
    ));
    let controller = &app.controllers[0].actions().next().unwrap().body;
    for body in [controller, &app.views[0].body] {
        assert!(calls_helper(body, "posts_path"));
        for name in [
            "journal_posts_path",
            "journal_one_posts_path",
            "journal_two_posts_path",
        ] {
            assert!(!calls_helper(body, name));
        }
    }
}

#[test]
fn isolated_controller_helpers_keep_their_own_return_types() {
    for (host_body, engine_body) in [
        ("1", "'engine'"),
        ("@caption ||= 1", "@caption ||= 'engine'"),
    ] {
        let mut tree = tree();
        put(
            &mut tree,
            "app/controllers/home_controller.rb",
            &format!(
                "class HomeController < ApplicationController; helper_method :caption; def caption; {host_body}; end; end"
            ),
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
            &format!(
                "class Blog::PostsController < ApplicationController; helper_method :caption; def caption; {engine_body}; end; end"
            ),
        );
        put(
            &mut tree,
            "app/views/home/index.html.erb",
            "<%= caption + 1 %>",
        );
        put(
            &mut tree,
            "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
            "<%= caption + '!' %>",
        );
        let mut app = app(tree);
        roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
        for (view, expected) in [
            ("home/index", roundhouse::ty::Ty::Int),
            ("blog/posts/index", roundhouse::ty::Ty::Str),
        ] {
            let body = &app
                .views
                .iter()
                .find(|v| v.name.as_str() == view)
                .unwrap()
                .body;
            assert_eq!(
                call_type(body, "caption"),
                Some(expected),
                "{view}: {host_body}"
            );
        }
        let errors: Vec<_> = roundhouse::analyze::diagnose(&app)
            .into_iter()
            .filter(|d| d.severity == roundhouse::analyze::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{errors:#?}");
    }
}

#[test]
fn isolated_controller_helpers_follow_inheritance_without_sibling_leaks() {
    let mut tree = tree();
    put(
        &mut tree,
        "app/controllers/application_controller.rb",
        "class ApplicationController < ActionController::Base; helper_method :caption; def caption; @caption ||= 7; end; end",
    );
    put(
        &mut tree,
        "app/controllers/home_controller.rb",
        "class HomeController < ApplicationController; helper_method :caption; def caption; 'host'; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/posts_controller.rb",
        "class Blog::PostsController < ApplicationController; def index; 'ok'; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/concerns/engine_caption.rb",
        "module EngineCaption; extend ActiveSupport::Concern; included do; helper_method :badge; end; def badge; @badge ||= 'engine'; end; end",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/controllers/blog/badges_controller.rb",
        "class Blog::BadgesController < ApplicationController; include EngineCaption; end",
    );
    put(
        &mut tree,
        "app/views/home/index.html.erb",
        "<%= caption %><%= badge %>",
    );
    put(
        &mut tree,
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
        "<%= caption %><%= badge %>",
    );
    let mut app = app(tree);
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let host = &app
        .views
        .iter()
        .find(|v| v.name.as_str() == "home/index")
        .unwrap()
        .body;
    let engine = &app
        .views
        .iter()
        .find(|v| v.name.as_str() == "blog/posts/index")
        .unwrap()
        .body;
    assert_eq!(call_type(host, "caption"), Some(roundhouse::ty::Ty::Str));
    assert_eq!(call_type(engine, "caption"), Some(roundhouse::ty::Ty::Int));
    assert_eq!(call_type(engine, "badge"), Some(roundhouse::ty::Ty::Str));
    assert!(call_type(host, "badge").unwrap().is_unknown());
}

#[test]
fn isolated_views_receive_the_complete_framework_surface() {
    let mut tree = tree();
    put(
        &mut tree,
        "app/models/user.rb",
        "class User < ApplicationRecord; devise :database_authenticatable; end",
    );
    for path in [
        "app/views/home/index.html.erb",
        "engines/not_the_namespace/app/views/blog/posts/index.html.erb",
    ] {
        put(&mut tree, path, "<%= request.path %><%= user_signed_in? %>");
    }
    let mut app = app(tree);
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    for view in &app.views {
        assert_eq!(
            call_type(&view.body, "request"),
            Some(roundhouse::ty::Ty::Class {
                id: roundhouse::ClassId("ActionDispatch::Request".into()),
                args: vec![],
            }),
            "{}",
            view.name,
        );
        assert_eq!(call_type(&view.body, "path"), Some(roundhouse::ty::Ty::Str));
        assert_eq!(
            call_type(&view.body, "user_signed_in?"),
            Some(roundhouse::ty::Ty::Bool)
        );
    }
}
