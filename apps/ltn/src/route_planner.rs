use geom::{Duration, Polygon};
use map_gui::tools::{
    DrawSimpleRoadLabels, InputWaypoints, TripManagement, TripManagementState, WaypointID,
};
use map_model::{PathV2, PathfinderCache};
use synthpop::{TripEndpoint, TripMode};
use widgetry::mapspace::World;
use widgetry::{
    Color, Drawable, EventCtx, GeomBatch, GfxCtx, Image, Line, Outcome, Panel, RoundedF64, Spinner,
    State, TextExt, Toggle, Widget,
};

use crate::components::{AppwidePanel, Mode};
use crate::{colors, App, Transition};

pub struct RoutePlanner {
    appwide_panel: AppwidePanel,
    left_panel: Panel,
    waypoints: InputWaypoints,
    files: TripManagement<App, RoutePlanner>,
    world: World<WaypointID>,
    show_main_roads: Drawable,
    draw_routes: Drawable,
    // TODO We could save the no-filter variations map-wide
    pathfinder_cache: PathfinderCache,
}

impl TripManagementState<App> for RoutePlanner {
    fn mut_files(&mut self) -> &mut TripManagement<App, Self> {
        &mut self.files
    }

    fn app_session_current_trip_name(app: &mut App) -> &mut Option<String> {
        &mut app.per_map.current_trip_name
    }

    fn sync_from_file_management(&mut self, ctx: &mut EventCtx, app: &mut App) {
        self.waypoints
            .overwrite(app, self.files.current.waypoints.clone());
        self.update_everything(ctx, app);
    }
}

impl RoutePlanner {
    pub fn new_state(ctx: &mut EventCtx, app: &mut App) -> Box<dyn State<App>> {
        if app.per_map.draw_all_road_labels.is_none() {
            app.per_map.draw_all_road_labels = Some(DrawSimpleRoadLabels::all_roads(
                ctx,
                app,
                colors::ROAD_LABEL,
            ));
        }

        // Fade all neighbourhood interiors, so it's very clear when a route cuts through
        let mut batch = GeomBatch::new();
        for info in app.partitioning().all_neighbourhoods().values() {
            batch.push(app.cs.fade_map_dark, info.block.polygon.clone());
        }

        let mut rp = RoutePlanner {
            appwide_panel: AppwidePanel::new(ctx, app, Mode::RoutePlanner),
            left_panel: Panel::empty(ctx),
            waypoints: InputWaypoints::new_max_2(app),
            files: TripManagement::new(app),
            world: World::unbounded(),
            show_main_roads: ctx.upload(batch),
            draw_routes: Drawable::empty(ctx),
            pathfinder_cache: PathfinderCache::new(),
        };

        if let Some(current_name) = &app.per_map.current_trip_name {
            rp.files.set_current(current_name);
        }
        rp.sync_from_file_management(ctx, app);

        Box::new(rp)
    }

    // Updates the panel and draw_routes
    fn update_everything(&mut self, ctx: &mut EventCtx, app: &mut App) {
        self.files.autosave(app);
        let results_widget = self.recalculate_paths(ctx, app);

        let contents = Widget::col(vec![
            Line("Plan a route").small_heading().into_widget(ctx),
            Widget::col(vec![
                self.files.get_panel_widget(ctx),
                Widget::horiz_separator(ctx, 1.0),
                self.waypoints.get_panel_widget(ctx).named("waypoints"),
            ]),
            if self.waypoints.get_waypoints().len() < 2 {
                Widget::nothing()
            } else {
                Widget::col(vec![
                    Widget::row(vec![
                        Line("Slow-down factor for main roads:")
                            .into_widget(ctx)
                            .centered_vert(),
                        Spinner::f64_widget(
                            ctx,
                            "main road penalty",
                            (1.0, 10.0),
                            app.session.main_road_penalty,
                            0.5,
                        ),
                        ctx.style()
                            .btn_plain
                            .icon("system/assets/tools/help.svg")
                            .tooltip(
                                "Increase to see how drivers may try to detour in heavy traffic",
                            )
                            .build_widget(ctx, "penalty instructions")
                            .align_right(),
                    ]),
                    Line("1 means free-flow traffic conditions")
                        .secondary()
                        .into_widget(ctx),
                ])
            },
            // Invisible separator
            GeomBatch::from(vec![(Color::CLEAR, Polygon::rectangle(0.1, 30.0))]).into_widget(ctx),
            results_widget.named("results"),
        ]);
        let mut panel =
            crate::components::LeftPanel::right_of_proposals(ctx, &self.appwide_panel, contents)
                // Hovering on waypoint cards
                .ignore_initial_events()
                .build(ctx);
        panel.restore(ctx, &self.left_panel);
        self.left_panel = panel;

        let mut world = World::bounded(app.per_map.map.get_bounds());
        self.waypoints.rebuild_world(ctx, &mut world, |x| x, 0);
        world.initialize_hover(ctx);
        world.rebuilt_during_drag(&self.world);
        self.world = world;
    }

    // Called when waypoints changed, but the number has stayed the same. Aka, the common case of a
    // waypoint being dragged. Does less work for speed.
    fn update_minimal(&mut self, ctx: &mut EventCtx, app: &mut App) {
        self.files.autosave(app);
        let results_widget = self.recalculate_paths(ctx, app);

        let mut world = World::bounded(app.per_map.map.get_bounds());
        self.waypoints.rebuild_world(ctx, &mut world, |x| x, 0);
        world.initialize_hover(ctx);
        world.rebuilt_during_drag(&self.world);
        self.world = world;

        self.left_panel.replace(ctx, "results", results_widget);

        // TODO This is the most expensive part. While we're dragging, can we just fade out the
        // cards or de-emphasize them somehow, and only do the recalculation when done?
        let waypoints_widget = self.waypoints.get_panel_widget(ctx);
        self.left_panel.replace(ctx, "waypoints", waypoints_widget);
    }

    // Returns a widget to display
    fn recalculate_paths(&mut self, ctx: &mut EventCtx, app: &App) -> Widget {
        if self.waypoints.get_waypoints().len() < 2 {
            self.draw_routes = Drawable::empty(ctx);
            return Widget::nothing();
        }

        let map = &app.per_map.map;

        let mut paths: Vec<(PathV2, Color)> = Vec::new();

        let driving_before_changes_time = {
            let mut total_time = Duration::ZERO;
            let mut params = app.per_map.routing_params_before_changes.clone();
            params.main_road_penalty = app.session.main_road_penalty;

            for pair in self.waypoints.get_waypoints().windows(2) {
                if let Some(path) = TripEndpoint::path_req(pair[0], pair[1], TripMode::Drive, map)
                    .and_then(|req| {
                        self.pathfinder_cache
                            .pathfind_with_params(map, req, params.clone())
                    })
                {
                    total_time += path.get_cost();
                    paths.push((path, *colors::PLAN_ROUTE_BEFORE));
                }
            }

            total_time
        };

        // The route respecting the filters
        let driving_after_changes_time = {
            let mut params = map.routing_params().clone();
            app.edits().update_routing_params(&mut params);
            params.main_road_penalty = app.session.main_road_penalty;

            let mut total_time = Duration::ZERO;
            let mut paths_after = Vec::new();
            for pair in self.waypoints.get_waypoints().windows(2) {
                if let Some(path) = TripEndpoint::path_req(pair[0], pair[1], TripMode::Drive, map)
                    .and_then(|req| {
                        self.pathfinder_cache
                            .pathfind_with_params(map, req, params.clone())
                    })
                {
                    total_time += path.get_cost();
                    paths_after.push((path, *colors::PLAN_ROUTE_AFTER));
                }
            }
            // To simplify colors, don't draw this path when it's the same as the baseline
            if total_time != driving_before_changes_time {
                paths.append(&mut paths_after);
            }

            total_time
        };

        let biking_time = if app.session.show_walking_cycling_routes {
            // No custom params, but don't use the map's built-in bike CH. Changes to one-way
            // streets haven't been reflected, and it's cheap enough to use Dijkstra's for
            // calculating one path at a time anyway.
            let mut total_time = Duration::ZERO;
            for pair in self.waypoints.get_waypoints().windows(2) {
                if let Some(path) = TripEndpoint::path_req(pair[0], pair[1], TripMode::Bike, map)
                    .and_then(|req| {
                        self.pathfinder_cache.pathfind_with_params(
                            map,
                            req,
                            map.routing_params().clone(),
                        )
                    })
                {
                    total_time += path.get_cost();
                    paths.push((path, *colors::PLAN_ROUTE_BIKE));
                }
            }
            total_time
        } else {
            Duration::ZERO
        };

        let walking_time = if app.session.show_walking_cycling_routes {
            // Same as above -- don't use the built-in CH.
            let mut total_time = Duration::ZERO;
            for pair in self.waypoints.get_waypoints().windows(2) {
                if let Some(path) = TripEndpoint::path_req(pair[0], pair[1], TripMode::Walk, map)
                    .and_then(|req| {
                        self.pathfinder_cache.pathfind_with_params(
                            map,
                            req,
                            map.routing_params().clone(),
                        )
                    })
                {
                    total_time += path.get_cost();
                    paths.push((path, *colors::PLAN_ROUTE_WALK));
                }
            }
            total_time
        } else {
            Duration::ZERO
        };

        self.draw_routes = map_gui::tools::draw_overlapping_paths(app, paths)
            .unzoomed
            .upload(ctx);

        Widget::col(vec![
            // TODO Circle icons
            Widget::row(vec![
                Image::from_path("system/assets/meters/car.svg")
                    .color(*colors::PLAN_ROUTE_BEFORE)
                    .into_widget(ctx),
                "Driving before any changes".text_widget(ctx),
                Line(driving_before_changes_time.to_rounded_string(0))
                    .into_widget(ctx)
                    .align_right(),
            ]),
            if driving_before_changes_time == driving_after_changes_time {
                Widget::row(vec![
                    Image::from_path("system/assets/meters/car.svg")
                        .color(*colors::PLAN_ROUTE_BEFORE)
                        .into_widget(ctx),
                    "Driving after changes".text_widget(ctx),
                    "Same".text_widget(ctx).align_right(),
                ])
            } else {
                Widget::row(vec![
                    Image::from_path("system/assets/meters/car.svg")
                        .color(*colors::PLAN_ROUTE_AFTER)
                        .into_widget(ctx),
                    "Driving after changes".text_widget(ctx),
                    Line(driving_after_changes_time.to_rounded_string(0))
                        .into_widget(ctx)
                        .align_right(),
                ])
            },
            if app.session.show_walking_cycling_routes {
                Widget::col(vec![
                    // TODO Is the tooltip that important? "This cycling route doesn't avoid
                    // high-stress roads or hills, and assumes an average 10mph pace"
                    Widget::row(vec![
                        Image::from_path("system/assets/meters/bike.svg")
                            .color(*colors::PLAN_ROUTE_BIKE)
                            .into_widget(ctx),
                        "Cycling".text_widget(ctx),
                        Line(biking_time.to_rounded_string(0))
                            .into_widget(ctx)
                            .align_right(),
                    ]),
                    Widget::row(vec![
                        Image::from_path("system/assets/meters/pedestrian.svg")
                            .color(*colors::PLAN_ROUTE_WALK)
                            .into_widget(ctx),
                        "Walking".text_widget(ctx),
                        Line(walking_time.to_rounded_string(0))
                            .into_widget(ctx)
                            .align_right(),
                    ]),
                ])
            } else {
                Widget::nothing()
            },
            // TODO Tooltip to explain how these routes remain direct?
            Toggle::checkbox(
                ctx,
                "Show walking & cycling route",
                None,
                app.session.show_walking_cycling_routes,
            ),
        ])
    }
}

impl State<App> for RoutePlanner {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        if let Some(t) =
            self.appwide_panel
                .event(ctx, app, &crate::save::PreserveState::Route, help)
        {
            return t;
        }
        if let Some(t) = app
            .session
            .layers
            .event(ctx, &app.cs, Mode::RoutePlanner, None)
        {
            return t;
        }

        let panel_outcome = self.left_panel.event(ctx);
        if let Outcome::Clicked(ref x) = panel_outcome {
            if let Some(t) = self.files.on_click(ctx, app, x) {
                // Bit hacky...
                if matches!(t, Transition::Keep) {
                    self.sync_from_file_management(ctx, app);
                }
                return t;
            }
            if x == "penalty instructions" {
                return Transition::Keep;
            }
            // Might be for waypoints
        }

        if let Outcome::Changed(ref x) = panel_outcome {
            if x == "main road penalty" {
                app.session.main_road_penalty =
                    self.left_panel.spinner::<RoundedF64>("main road penalty").0;
                self.update_everything(ctx, app);
            } else if x == "Show walking & cycling route" {
                app.session.show_walking_cycling_routes =
                    self.left_panel.is_checked("Show walking & cycling route");
                self.update_everything(ctx, app);
            }
        }

        let waypoints_before = self.waypoints.get_waypoints().len();
        if self
            .waypoints
            .event(app, panel_outcome, self.world.event(ctx))
        {
            // Sync from waypoints to file management
            // TODO Maaaybe this directly live in the InputWaypoints system?
            self.files.current.waypoints = self.waypoints.get_waypoints();

            if self.waypoints.get_waypoints().len() == waypoints_before {
                self.update_minimal(ctx, app);
            } else {
                self.update_everything(ctx, app);
            }
        }

        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, app: &App) {
        self.appwide_panel.draw(g);
        self.left_panel.draw(g);
        app.session.layers.draw(g, app);

        g.redraw(&self.show_main_roads);
        self.world.draw(g);
        self.draw_routes.draw(g);
        app.per_map.draw_all_road_labels.as_ref().unwrap().draw(g);
        app.per_map.draw_all_filters.draw(g);
        app.per_map.draw_poi_icons.draw(g);
    }

    fn recreate(&mut self, ctx: &mut EventCtx, app: &mut App) -> Box<dyn State<App>> {
        Self::new_state(ctx, app)
    }
}

fn help() -> Vec<&'static str> {
    vec![
        "You can test how different driving routes are affected by proposed LTNs.",
        "",
        "The fastest route may not cut through neighbourhoods normally,",
        "but you can adjust the slow-down factor to mimic rush hour conditions",
    ]
}
