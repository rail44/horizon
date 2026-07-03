use floem::{
    context::PaintCx,
    peniko::{kurbo::Rect, Color},
};
use floem_renderer::Renderer;

use super::layout::BlockElement;

pub(super) fn draw_block_element(
    cx: &mut PaintCx,
    block: BlockElement,
    cell_rect: Rect,
    fg: [u8; 3],
) {
    let color = Color::rgb8(fg[0], fg[1], fg[2]);
    match block {
        BlockElement::Full => cx.fill(&expanded_rect(cell_rect), color, 0.0),
        BlockElement::UpperFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x0,
                cell_rect.y0,
                cell_rect.x1,
                cell_rect.y0 + cell_rect.height() * fraction(eighths),
            );
            cx.fill(&expanded_rect(rect), color, 0.0);
        }
        BlockElement::LowerFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x0,
                cell_rect.y1 - cell_rect.height() * fraction(eighths),
                cell_rect.x1,
                cell_rect.y1,
            );
            cx.fill(&expanded_rect(rect), color, 0.0);
        }
        BlockElement::LeftFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x0,
                cell_rect.y0,
                cell_rect.x0 + cell_rect.width() * fraction(eighths),
                cell_rect.y1,
            );
            cx.fill(&expanded_rect(rect), color, 0.0);
        }
        BlockElement::RightFraction(eighths) => {
            let rect = Rect::new(
                cell_rect.x1 - cell_rect.width() * fraction(eighths),
                cell_rect.y0,
                cell_rect.x1,
                cell_rect.y1,
            );
            cx.fill(&expanded_rect(rect), color, 0.0);
        }
        BlockElement::Quadrants {
            upper_left,
            upper_right,
            lower_left,
            lower_right,
        } => {
            let mid_x = midpoint(cell_rect.x0, cell_rect.x1);
            let mid_y = midpoint(cell_rect.y0, cell_rect.y1);
            if upper_left {
                cx.fill(
                    &expanded_rect(Rect::new(cell_rect.x0, cell_rect.y0, mid_x, mid_y)),
                    color,
                    0.0,
                );
            }
            if upper_right {
                cx.fill(
                    &expanded_rect(Rect::new(mid_x, cell_rect.y0, cell_rect.x1, mid_y)),
                    color,
                    0.0,
                );
            }
            if lower_left {
                cx.fill(
                    &expanded_rect(Rect::new(cell_rect.x0, mid_y, mid_x, cell_rect.y1)),
                    color,
                    0.0,
                );
            }
            if lower_right {
                cx.fill(
                    &expanded_rect(Rect::new(mid_x, mid_y, cell_rect.x1, cell_rect.y1)),
                    color,
                    0.0,
                );
            }
        }
    }
}

fn midpoint(start: f64, end: f64) -> f64 {
    start + (end - start) / 2.0
}

fn fraction(eighths: u8) -> f64 {
    eighths.clamp(1, 8) as f64 / 8.0
}

pub(super) fn expanded_rect(rect: Rect) -> Rect {
    const OVERLAP: f64 = 0.5;
    Rect::new(
        rect.x0 - OVERLAP,
        rect.y0 - OVERLAP,
        rect.x1 + OVERLAP,
        rect.y1 + OVERLAP,
    )
}
