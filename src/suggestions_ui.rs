use std::path::PathBuf;

use bytesize::ByteSize;
use eframe::egui;

use crate::suggestions::{SafetyLevel, SuggestionReport};

/// Actions produced by the suggestions UI.
pub enum SuggestionAction {
    ToggleGroup(usize),
    TrashItem(PathBuf),
    TrashGroup(usize),
}

/// Render the Smart Cleanup suggestions panel.
pub fn render_suggestions(ui: &mut egui::Ui, report: &SuggestionReport) -> Vec<SuggestionAction> {
    let mut actions = Vec::new();

    if report.groups.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.heading("No cleanup suggestions found");
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Your scanned directory looks clean.")
                    .weak()
                    .size(14.0),
            );
        });
        return actions;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(16.0);

            // Summary header
            ui.vertical_centered(|ui| {
                let size_str = ByteSize::b(report.total_reclaimable).to_string();
                ui.heading(
                    egui::RichText::new(format!("You can reclaim {size_str}"))
                        .size(22.0)
                        .strong(),
                );
                ui.add_space(4.0);
                let group_count = report.groups.len();
                let item_count: usize = report.groups.iter().map(|g| g.items.len()).sum();
                ui.label(
                    egui::RichText::new(format!(
                        "{item_count} items across {group_count} categories"
                    ))
                    .weak()
                    .size(14.0),
                );
            });

            ui.add_space(20.0);

            // Category cards
            let card_width = (ui.available_width() - 32.0).min(600.0);

            for (group_idx, group) in report.groups.iter().enumerate() {
                let cat = group.category;
                let safety = cat.safety();

                // Center the card
                let avail = ui.available_width();
                let indent = ((avail - card_width) / 2.0).max(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(indent);

                    egui::Frame::group(ui.style())
                        .inner_margin(16.0)
                        .corner_radius(8.0)
                        .show(ui, |ui| {
                            ui.set_width(card_width);

                            // Header row: icon + name + size + safety badge
                            ui.horizontal(|ui| {
                                // Icon and category name
                                ui.label(egui::RichText::new(cat.icon()).size(20.0));
                                ui.label(egui::RichText::new(cat.label()).strong().size(16.0));

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Clean button
                                        let clean_btn = ui.add(
                                            egui::Button::new(
                                                egui::RichText::new(format!(
                                                    "Clean {}",
                                                    ByteSize::b(group.total_size)
                                                ))
                                                .size(13.0),
                                            )
                                            .corner_radius(4.0),
                                        );
                                        if clean_btn.clicked() {
                                            actions.push(SuggestionAction::TrashGroup(group_idx));
                                        }

                                        // Safety badge
                                        let badge_text = safety.label();
                                        let badge_color = safety.color();
                                        let badge = egui::Frame::NONE
                                            .inner_margin(egui::Margin::symmetric(8, 2))
                                            .corner_radius(10.0)
                                            .fill(badge_color.linear_multiply(0.2))
                                            .stroke(egui::Stroke::new(1.0, badge_color));
                                        badge.show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(badge_text)
                                                    .color(badge_color)
                                                    .size(11.0),
                                            );
                                        });
                                    },
                                );
                            });

                            // Description
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(cat.description()).weak().size(12.0));

                            // Size bar
                            ui.add_space(8.0);
                            let bar_height = 6.0;
                            let (bar_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), bar_height),
                                egui::Sense::hover(),
                            );
                            let painter = ui.painter();
                            painter.rect_filled(bar_rect, 3.0, ui.visuals().extreme_bg_color);

                            // Fill proportional to this group vs total
                            let fraction = if report.total_reclaimable > 0 {
                                group.total_size as f32 / report.total_reclaimable as f32
                            } else {
                                0.0
                            };
                            let fill_w = (bar_rect.width() * fraction.clamp(0.0, 1.0)).max(2.0);
                            let fill_rect = egui::Rect::from_min_size(
                                bar_rect.min,
                                egui::vec2(fill_w, bar_height),
                            );
                            let bar_color = match safety {
                                SafetyLevel::Safe => egui::Color32::from_rgb(39, 174, 96),
                                SafetyLevel::Caution => egui::Color32::from_rgb(220, 150, 50),
                            };
                            painter.rect_filled(fill_rect, 3.0, bar_color);

                            // Item count + total size
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} \u{2014} {} items",
                                        ByteSize::b(group.total_size),
                                        group.items.len()
                                    ))
                                    .size(13.0),
                                );
                            });

                            // Expand/collapse toggle
                            ui.add_space(4.0);
                            let toggle_text = if group.expanded {
                                "Hide details \u{25B2}"
                            } else {
                                "Show details \u{25BC}"
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(toggle_text).weak().size(12.0),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                actions.push(SuggestionAction::ToggleGroup(group_idx));
                            }

                            // Expanded item list
                            if group.expanded {
                                ui.add_space(4.0);
                                ui.separator();
                                ui.add_space(4.0);

                                let max_show = 20;
                                let show_count = group.items.len().min(max_show);

                                for item in &group.items[..show_count] {
                                    ui.horizontal(|ui| {
                                        // Trash button
                                        let trash_btn = ui.add(
                                            egui::Button::new(
                                                egui::RichText::new("\u{1F5D1}").size(12.0),
                                            )
                                            .frame(false),
                                        );
                                        if trash_btn.clicked() {
                                            actions.push(SuggestionAction::TrashItem(
                                                item.path.clone(),
                                            ));
                                        }

                                        // Path (truncated)
                                        let display_path = item.path.display().to_string();
                                        let truncated = if display_path.len() > 60 {
                                            format!(
                                                "...{}",
                                                &display_path[display_path.len() - 57..]
                                            )
                                        } else {
                                            display_path
                                        };
                                        ui.label(
                                            egui::RichText::new(truncated).monospace().size(12.0),
                                        );

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    egui::RichText::new(
                                                        ByteSize::b(item.size).to_string(),
                                                    )
                                                    .monospace()
                                                    .size(12.0),
                                                );
                                            },
                                        );
                                    });
                                }

                                if group.items.len() > max_show {
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "... and {} more items",
                                            group.items.len() - max_show
                                        ))
                                        .weak()
                                        .size(12.0),
                                    );
                                }
                            }
                        });
                });

                ui.add_space(8.0);
            }

            ui.add_space(16.0);
        });

    actions
}
