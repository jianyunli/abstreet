use std::collections::HashMap;

use abstutil::{prettyprint_usize, Counter, Timer};
use geom::{Duration, Polygon};
use map_gui::colors::ColorSchemeChoice;
use map_gui::tools::{cmp_count, ColorNetwork};
use map_gui::{AppLike, ID};
use map_model::{
    DirectedRoadID, Direction, PathConstraints, PathRequest, PathStepV2, Pathfinder, RoadID,
    RoutingParams, NORMAL_LANE_THICKNESS,
};
use synthpop::{TripEndpoint, TripMode};
use widgetry::mapspace::ToggleZoomed;
use widgetry::{
    Color, Drawable, EventCtx, GeomBatch, GfxCtx, HorizontalAlignment, Key, Line, Outcome, Panel,
    RoundedF64, Spinner, State, Text, TextExt, VerticalAlignment, Widget,
};

use crate::app::{App, Transition};
use crate::common::CommonState;

/// See how live-tuned routing parameters affect a single request.
pub struct RouteExplorer {
    panel: Panel,
    start: TripEndpoint,
    // (endpoint, confirmed, render the paths to it)
    goal: Option<(TripEndpoint, bool, Drawable)>,
}

impl RouteExplorer {
    pub fn new_state(ctx: &mut EventCtx, app: &App, start: TripEndpoint) -> Box<dyn State<App>> {
        Box::new(RouteExplorer {
            start,
            goal: None,
            panel: Panel::new_builder(Widget::col(vec![
                Widget::row(vec![
                    Line("Route explorer").small_heading().into_widget(ctx),
                    ctx.style().btn_close_widget(ctx),
                ]),
                ctx.style()
                    .btn_outline
                    .text("All routes")
                    .hotkey(Key::A)
                    .build_def(ctx),
                params_to_controls(ctx, TripMode::Bike, app.primary.map.routing_params())
                    .named("params"),
            ]))
            .aligned(HorizontalAlignment::Right, VerticalAlignment::Top)
            .build(ctx),
        })
    }

    fn recalc_paths(&mut self, ctx: &mut EventCtx, app: &App) {
        let (mode, params) = controls_to_params(&self.panel);

        if let Some((ref goal, _, ref mut preview)) = self.goal {
            *preview = Drawable::empty(ctx);
            if let Some(polygon) = TripEndpoint::path_req(self.start, *goal, mode, &app.primary.map)
                .and_then(|req| {
                    Pathfinder::new_dijkstra(
                        &app.primary.map,
                        params,
                        vec![req.constraints],
                        &mut Timer::throwaway(),
                    )
                    .pathfind_v2(req, &app.primary.map)
                })
                .and_then(|path| path.into_v1(&app.primary.map).ok())
                .and_then(|path| path.trace(&app.primary.map))
                .map(|pl| pl.make_polygons(NORMAL_LANE_THICKNESS))
            {
                *preview = GeomBatch::from(vec![(Color::PURPLE, polygon)]).upload(ctx);
            }
        }
    }
}

impl State<App> for RouteExplorer {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        ctx.canvas_movement();

        match self.panel.event(ctx) {
            Outcome::Clicked(x) => match x.as_ref() {
                "close" => {
                    return Transition::Pop;
                }
                "bikes" => {
                    let controls =
                        params_to_controls(ctx, TripMode::Bike, app.primary.map.routing_params());
                    self.panel.replace(ctx, "params", controls);
                    self.recalc_paths(ctx, app);
                }
                "cars" => {
                    let controls =
                        params_to_controls(ctx, TripMode::Drive, app.primary.map.routing_params());
                    self.panel.replace(ctx, "params", controls);
                    self.recalc_paths(ctx, app);
                }
                "pedestrians" => {
                    let controls =
                        params_to_controls(ctx, TripMode::Walk, app.primary.map.routing_params());
                    self.panel.replace(ctx, "params", controls);
                    self.recalc_paths(ctx, app);
                }
                "All routes" => {
                    return Transition::Replace(AllRoutesExplorer::new_state(ctx, app));
                }
                _ => unreachable!(),
            },
            Outcome::Changed(_) => {
                self.recalc_paths(ctx, app);
            }
            _ => {}
        }

        if self
            .goal
            .as_ref()
            .map(|(_, confirmed, _)| *confirmed)
            .unwrap_or(false)
        {
            return Transition::Keep;
        }

        if ctx.redo_mouseover() {
            app.primary.current_selection = app.mouseover_unzoomed_everything(ctx);
            if match app.primary.current_selection {
                Some(ID::Intersection(i)) => !app.primary.map.get_i(i).is_border(),
                Some(ID::Building(_)) => false,
                _ => true,
            } {
                app.primary.current_selection = None;
            }
        }
        if let Some(hovering) = match app.primary.current_selection {
            Some(ID::Intersection(i)) => Some(TripEndpoint::Border(i)),
            Some(ID::Building(b)) => Some(TripEndpoint::Building(b)),
            None => None,
            _ => unreachable!(),
        } {
            if self.start != hovering {
                if self
                    .goal
                    .as_ref()
                    .map(|(to, _, _)| to != &hovering)
                    .unwrap_or(true)
                {
                    self.goal = Some((hovering, false, Drawable::empty(ctx)));
                    self.recalc_paths(ctx, app);
                }
            } else {
                self.goal = None;
            }
        } else {
            self.goal = None;
        }

        if let Some((_, ref mut confirmed, _)) = self.goal {
            if app.per_obj.left_click(ctx, "end here") {
                app.primary.current_selection = None;
                *confirmed = true;
            }
        }

        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, app: &App) {
        self.panel.draw(g);
        CommonState::draw_osd(g, app);

        g.draw_polygon(
            Color::BLUE.alpha(0.8),
            match self.start {
                TripEndpoint::Border(i) => app.primary.map.get_i(i).polygon.clone(),
                TripEndpoint::Building(b) => app.primary.map.get_b(b).polygon.clone(),
                TripEndpoint::SuddenlyAppear(_) => unreachable!(),
            },
        );
        if let Some((ref endpt, _, ref draw)) = self.goal {
            g.draw_polygon(
                Color::GREEN.alpha(0.8),
                match endpt {
                    TripEndpoint::Border(i) => app.primary.map.get_i(*i).polygon.clone(),
                    TripEndpoint::Building(b) => app.primary.map.get_b(*b).polygon.clone(),
                    TripEndpoint::SuddenlyAppear(_) => unreachable!(),
                },
            );
            g.redraw(draw);
        }
    }
}

fn params_to_controls(ctx: &mut EventCtx, mode: TripMode, params: &RoutingParams) -> Widget {
    let mut rows = vec![Widget::custom_row(vec![
        ctx.style()
            .btn_plain
            .icon("system/assets/meters/bike.svg")
            .disabled(mode == TripMode::Bike)
            .build_widget(ctx, "bikes"),
        ctx.style()
            .btn_plain
            .icon("system/assets/meters/car.svg")
            .disabled(mode == TripMode::Drive)
            .build_widget(ctx, "cars"),
        ctx.style()
            .btn_plain
            .icon("system/assets/meters/pedestrian.svg")
            .disabled(mode == TripMode::Walk)
            .build_widget(ctx, "pedestrians"),
    ])
    .evenly_spaced()];
    if mode == TripMode::Drive || mode == TripMode::Bike {
        rows.push(Widget::row(vec![
            "Unprotected turn penalty:"
                .text_widget(ctx)
                .margin_right(20),
            Spinner::widget(
                ctx,
                "unprotected_turn_penalty",
                (Duration::seconds(1.0), Duration::seconds(100.0)),
                params.unprotected_turn_penalty,
                Duration::seconds(1.0),
            ),
        ]));
    }
    if mode == TripMode::Bike {
        rows.push(Widget::row(vec![
            "Bike lane penalty:".text_widget(ctx).margin_right(20),
            Spinner::f64_widget(
                ctx,
                "bike_lane_penalty",
                (0.0, 2.0),
                params.bike_lane_penalty,
                0.1,
            ),
        ]));
        rows.push(Widget::row(vec![
            "Bus lane penalty:".text_widget(ctx).margin_right(20),
            Spinner::f64_widget(
                ctx,
                "bus_lane_penalty",
                (0.0, 2.0),
                params.bus_lane_penalty,
                0.1,
            ),
        ]));
        rows.push(Widget::row(vec![
            "Driving lane penalty:".text_widget(ctx).margin_right(20),
            Spinner::f64_widget(
                ctx,
                "driving_lane_penalty",
                (0.0, 2.0),
                params.driving_lane_penalty,
                0.1,
            ),
        ]));
        rows.push(Widget::row(vec![
            "Avoid steep inclines (>= 8%):"
                .text_widget(ctx)
                .margin_right(20),
            Spinner::f64_widget(
                ctx,
                "avoid_steep_incline_penalty",
                (0.0, 2.0),
                params.avoid_steep_incline_penalty,
                0.1,
            ),
        ]));
        rows.push(Widget::row(vec![
            "Avoid high-stress roads:".text_widget(ctx).margin_right(20),
            Spinner::f64_widget(
                ctx,
                "avoid_high_stress",
                (0.0, 2.0),
                params.avoid_high_stress,
                0.1,
            ),
        ]));
    }
    Widget::col(rows)
}

fn controls_to_params(panel: &Panel) -> (TripMode, RoutingParams) {
    let mut params = RoutingParams::default();
    if !panel.is_button_enabled("cars") {
        params.unprotected_turn_penalty = panel.spinner("unprotected_turn_penalty");
        return (TripMode::Drive, params);
    }
    if !panel.is_button_enabled("pedestrians") {
        return (TripMode::Walk, params);
    }
    params.unprotected_turn_penalty = panel.spinner("unprotected_turn_penalty");
    params.bike_lane_penalty = panel.spinner::<RoundedF64>("bike_lane_penalty").0;
    params.bus_lane_penalty = panel.spinner::<RoundedF64>("bus_lane_penalty").0;
    params.driving_lane_penalty = panel.spinner::<RoundedF64>("driving_lane_penalty").0;
    params.avoid_steep_incline_penalty =
        panel.spinner::<RoundedF64>("avoid_steep_incline_penalty").0;
    params.avoid_high_stress = panel.spinner::<RoundedF64>("avoid_high_stress").0;
    (TripMode::Bike, params)
}

/// See how live-tuned routing parameters affect all requests for the current scenario.
struct AllRoutesExplorer {
    panel: Panel,
    requests: Vec<PathRequest>,
    baseline_counts: Counter<RoadID>,

    current_counts: Counter<RoadID>,
    draw: ToggleZoomed,
    tooltip: Option<Text>,
}

impl AllRoutesExplorer {
    fn new_state(ctx: &mut EventCtx, app: &mut App) -> Box<dyn State<App>> {
        // Tuning the differential scale is hard enough; always use day mode.
        app.change_color_scheme(ctx, ColorSchemeChoice::DayMode);

        let (requests, baseline_counts) =
            ctx.loading_screen("calculate baseline paths", |_, timer| {
                let map = &app.primary.map;
                let requests = timer
                    .parallelize(
                        "predict route requests",
                        app.primary.sim.all_trip_info(),
                        |(_, trip)| TripEndpoint::path_req(trip.start, trip.end, trip.mode, map),
                    )
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                let baseline_counts = calculate_demand(app, map.get_pathfinder(), &requests, timer);
                (requests, baseline_counts)
            });
        let current_counts = baseline_counts.clone();

        // Start by showing the original counts, not relative to anything
        let mut colorer = ColorNetwork::new(app);
        colorer.ranked_roads(current_counts.clone(), &app.cs.good_to_bad_red);

        Box::new(AllRoutesExplorer {
            panel: Panel::new_builder(Widget::col(vec![
                Widget::row(vec![
                    Line("All routes explorer").small_heading().into_widget(ctx),
                    ctx.style().btn_close_widget(ctx),
                ]),
                format!("{} total requests", prettyprint_usize(requests.len())).text_widget(ctx),
                params_to_controls(ctx, TripMode::Bike, app.primary.map.routing_params())
                    .named("params"),
                ctx.style()
                    .btn_outline
                    .text("Calculate differential demand")
                    .build_def(ctx),
            ]))
            .aligned(HorizontalAlignment::Right, VerticalAlignment::Top)
            .build(ctx),
            requests,
            baseline_counts,
            current_counts,
            draw: colorer.build(ctx),
            tooltip: None,
        })
    }
}

impl State<App> for AllRoutesExplorer {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        ctx.canvas_movement();

        if let Outcome::Clicked(x) = self.panel.event(ctx) {
            match x.as_ref() {
                "close" => {
                    return Transition::Pop;
                }
                "bikes" => {
                    let controls =
                        params_to_controls(ctx, TripMode::Bike, app.primary.map.routing_params());
                    self.panel.replace(ctx, "params", controls);
                }
                "cars" => {
                    let controls =
                        params_to_controls(ctx, TripMode::Drive, app.primary.map.routing_params());
                    self.panel.replace(ctx, "params", controls);
                }
                "pedestrians" => {
                    let controls =
                        params_to_controls(ctx, TripMode::Walk, app.primary.map.routing_params());
                    self.panel.replace(ctx, "params", controls);
                }
                "Calculate differential demand" => {
                    ctx.loading_screen(
                        "calculate differential demand due to routing params",
                        |ctx, timer| {
                            let (_, params) = controls_to_params(&self.panel);
                            let pathfinder = Pathfinder::new_ch(
                                &app.primary.map,
                                params,
                                PathConstraints::all(),
                                timer,
                            );
                            self.current_counts =
                                calculate_demand(app, &pathfinder, &self.requests, timer);

                            // Calculate the difference
                            let mut colorer = ColorNetwork::new(app);
                            // TODO If this works well, promote it alongside DivergingScale
                            let more = &app.cs.good_to_bad_red;
                            let less = &app.cs.good_to_bad_green;
                            let comparisons = self
                                .baseline_counts
                                .clone()
                                .compare(self.current_counts.clone());
                            // Find the biggest gain/loss
                            let diff = comparisons
                                .iter()
                                .map(|(_, after, before)| {
                                    ((*after as isize) - (*before as isize)).abs() as usize
                                })
                                .max()
                                .unwrap_or(0) as f64;
                            for (r, before, after) in comparisons {
                                match after.cmp(&before) {
                                    std::cmp::Ordering::Less => {
                                        colorer.add_r(r, less.eval((before - after) as f64 / diff));
                                    }
                                    std::cmp::Ordering::Greater => {
                                        colorer.add_r(r, more.eval((after - before) as f64 / diff));
                                    }
                                    std::cmp::Ordering::Equal => {}
                                }
                            }
                            self.draw = colorer.build(ctx);
                        },
                    );
                }
                _ => unreachable!(),
            }
        }

        if ctx.redo_mouseover() {
            self.tooltip = None;
            if let Some(ID::Road(r)) = app.mouseover_unzoomed_roads_and_intersections(ctx) {
                let baseline = self.baseline_counts.get(r);
                let current = self.current_counts.get(r);
                let mut txt = Text::new();
                cmp_count(&mut txt, baseline, current);
                txt.add_line(format!("{} baseline", prettyprint_usize(baseline)));
                txt.add_line(format!("{} now", prettyprint_usize(current)));
                self.tooltip = Some(txt);
            }
        }

        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, app: &App) {
        self.panel.draw(g);
        CommonState::draw_osd(g, app);
        self.draw.draw(g);
        if let Some(ref txt) = self.tooltip {
            g.draw_mouse_tooltip(txt.clone());
        }
    }
}

fn calculate_demand(
    app: &App,
    pathfinder: &Pathfinder,
    requests: &[PathRequest],
    timer: &mut Timer,
) -> Counter<RoadID> {
    let map = &app.primary.map;
    let paths = timer
        .parallelize("pathfind", requests.to_vec(), |req| {
            pathfinder.pathfind_v2(req, map)
        })
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut counter = Counter::new();
    timer.start_iter("compute demand", paths.len());
    for path in paths {
        timer.next();
        for step in path.get_steps() {
            if let PathStepV2::Along(dr) | PathStepV2::Contraflow(dr) = step {
                counter.inc(dr.road);
            }
        }
    }
    counter
}

/// Evaluate why an alternative path wasn't chosen, by showing the cost to reach every road from
/// one start.
pub struct PathCostDebugger {
    draw_path: Drawable,
    costs: HashMap<DirectedRoadID, Duration>,
    tooltip: Option<Text>,
    panel: Panel,
}

impl PathCostDebugger {
    pub fn maybe_new(
        ctx: &mut EventCtx,
        app: &App,
        req: PathRequest,
        draw_path: Polygon,
    ) -> Option<Box<dyn State<App>>> {
        let (full_cost, all_costs) = app.primary.map.all_costs_from(req)?;
        let mut batch = GeomBatch::new();
        // Highlight all directed roads with a cost less than the cost of the chosen path. This
        // more or less shows "alternatives considered"; the boundary becomes the point where the
        // chosen path really did win.
        for (dr, cost) in &all_costs {
            if *cost <= full_cost {
                if let Ok(p) = app
                    .primary
                    .map
                    .get_r(dr.road)
                    .get_half_polygon(dr.dir, &app.primary.map)
                {
                    batch.push(Color::BLUE.alpha(0.5), p);
                }
            }
        }
        batch.push(Color::PURPLE, draw_path);

        Some(Box::new(PathCostDebugger {
            draw_path: ctx.upload(batch),
            costs: all_costs,
            tooltip: None,
            panel: Panel::new_builder(Widget::col(vec![
                Widget::row(vec![
                    Line("Path cost debugger").small_heading().into_widget(ctx),
                    ctx.style().btn_close_widget(ctx),
                ]),
                format!("Cost of chosen path: {}", full_cost).text_widget(ctx),
            ]))
            .aligned(HorizontalAlignment::Right, VerticalAlignment::Top)
            .build(ctx),
        }))
    }
}

impl State<App> for PathCostDebugger {
    fn event(&mut self, ctx: &mut EventCtx, app: &mut App) -> Transition {
        ctx.canvas_movement();

        if ctx.redo_mouseover() {
            self.tooltip = None;
            if let Some(ID::Road(r)) = app.mouseover_unzoomed_roads_and_intersections(ctx) {
                // TODO In lieu of mousing over each half of a road, just show both costs.
                let mut txt = Text::new();
                for dir in [Direction::Fwd, Direction::Back] {
                    if let Some(cost) = self.costs.get(&DirectedRoadID { road: r, dir }) {
                        txt.add_line(format!("Cost {:?}: {}", dir, cost));
                    } else {
                        txt.add_line(format!("No cost {:?}", dir));
                    }
                }
                self.tooltip = Some(txt);
            }
        }

        if let Outcome::Clicked(x) = self.panel.event(ctx) {
            match x.as_ref() {
                "close" => {
                    return Transition::Pop;
                }
                _ => unreachable!(),
            }
        }

        Transition::Keep
    }

    fn draw(&self, g: &mut GfxCtx, _: &App) {
        self.panel.draw(g);
        g.redraw(&self.draw_path);
        if let Some(ref txt) = self.tooltip {
            g.draw_mouse_tooltip(txt.clone());
        }
    }
}
