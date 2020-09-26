use crate::error::{ViuError, ViuResult};
use crate::printer::Printer;
use crate::Config;

use ansi_colours::ansi256_from_rgb;
use image::{DynamicImage, GenericImageView, Rgba};
use std::io::Write;
use termcolor::{Buffer, BufferWriter, Color, ColorChoice, ColorSpec, WriteColor};

use crossterm::cursor::{position, MoveRight, MoveTo, MoveToNextLine, MoveToPreviousLine};
use crossterm::execute;
use crossterm::tty::IsTty;

const UPPER_HALF_BLOCK: &str = "\u{2580}";
const LOWER_HALF_BLOCK: &str = "\u{2584}";

pub struct BlockPrinter {}

impl Printer for BlockPrinter {
    fn print(img: &DynamicImage, config: &Config) -> ViuResult {
        // there are two types of buffers in this function:
        // - stdout: Buffer, which is from termcolor crate. Used to buffer all writing
        //   required to print a single image or frame. Flushed once at the end of the function
        // - buffer: Vec<ColorSpec>, which stores back- and foreground colors for a
        //   maximum of 1 row of blocks, i.e 2 rows of pixels. Flushed every 2 pixel rows of the images
        // all mentions of buffer below refer to the latter
        let stdout = BufferWriter::stdout(ColorChoice::Always);
        let mut out_buffer = stdout.buffer();

        // Only make note of cursor position in tty. Otherwise, it disturbes output in tools like `head`, for example.
        let cursor_pos = if !config.absolute_offset && std::io::stdout().is_tty() {
            position().ok()
        } else {
            None
        };

        // adjust y offset
        if config.absolute_offset {
            if config.y >= 0 {
                // If absolute_offset, move to (0,y).
                execute!(out_buffer, MoveTo(0, config.y as u16))?;
            } else {
                //Negative values do not make sense.
                return Err(ViuError::InvalidConfiguration(
                    "absolute_offset is true but y offset is negative".to_owned(),
                ));
            }
        } else if config.y < 0 {
            // MoveUp if negative
            execute!(out_buffer, MoveToPreviousLine(-config.y as u16))?;
        } else {
            // Move down y lines
            for _ in 0..config.y {
                // writeln! is used instead of MoveDown to force scrolldown TODO: is it?
                writeln!(out_buffer)?;
            }
        }

        let (width, _) = img.dimensions();

        let mut curr_col_px = 0;
        let mut curr_row_px = 0;
        let mut row_buffer: Vec<ColorSpec> = Vec::with_capacity(width as usize);
        let mut mode = Mode::Top;

        // iterate pixels and fill a buffer that contains 2 rows of pixels
        // 2 rows translate to 1 row in the terminal by using half block, foreground and background
        // colors
        for pixel in img.pixels() {
            // if the alpha of the pixel is 0, print a predefined pixel based on the position in order
            // to mimic the chess board background. If the transparent option was given, instead print
            // nothing.
            let color = if is_pixel_transparent(pixel) {
                if config.transparent {
                    None
                } else {
                    Some(get_transparency_color(
                        curr_row_px,
                        curr_col_px,
                        config.truecolor,
                    ))
                }
            } else {
                Some(get_color_from_pixel(pixel, config.truecolor))
            };

            if mode == Mode::Top {
                let mut c = ColorSpec::new();
                c.set_bg(color);
                row_buffer.push(c);
            } else {
                let colorspec_to_upg = &mut row_buffer[curr_col_px as usize];
                colorspec_to_upg.set_fg(color);
            }

            curr_col_px += 1;
            // if the buffer is full start adding the second row of pixels
            if row_buffer.len() == width as usize {
                if mode == Mode::Top {
                    mode = Mode::Bottom;
                    curr_col_px = 0;
                    curr_row_px += 1;
                }
                // only if the second row is completed, flush the buffer and start again
                else if curr_col_px == width {
                    curr_col_px = 0;
                    curr_row_px += 1;

                    // move right if x offset is specified
                    if config.x > 0 {
                        execute!(out_buffer, MoveRight(config.x))?;
                    }

                    // flush the row_buffer into out_buffer
                    fill_out_buffer(&mut row_buffer, &mut out_buffer, false)?;

                    // write the line to stdout
                    print_buffer(&stdout, &mut out_buffer)?;

                    mode = Mode::Top;
                } else {
                    // in the middle of the second row, more iterations are required
                }
            }
        }

        // buffer will be flushed if the image has an odd height
        if !row_buffer.is_empty() {
            fill_out_buffer(&mut row_buffer, &mut out_buffer, true)?;
        }

        // if the cursor has gone up while printing the image (due to negative y offset),
        // bring it back down to its lowest position. Forces the cursor to be below everything
        // printed when the method has been called more than once.
        if !config.absolute_offset && std::io::stdout().is_tty() {
            if let Some((_, pos_y)) = cursor_pos {
                let (_, new_pos_y) = position()?;
                if pos_y > new_pos_y {
                    execute!(out_buffer, MoveToNextLine(pos_y - new_pos_y))?;
                };
            }
        };

        // do a final write to stdout, i.e flush
        print_buffer(&stdout, &mut out_buffer)
    }
}

// Send out_buffer to stdout. Empties it when it's done
fn print_buffer(stdout: &BufferWriter, out_buffer: &mut Buffer) -> ViuResult {
    match stdout.print(out_buffer) {
        Ok(_) => {
            out_buffer.clear();
            Ok(())
        }
        Err(e) => match e.kind() {
            // Ignore broken pipe errors. They arise when piping output to `head`, for example,
            // and panic is not desired.
            std::io::ErrorKind::BrokenPipe => Ok(()),
            _ => Err(ViuError::IO(e)),
        },
    }
}

// Translates the row_buffer, containing colors, into the out_buffer which will be flushed to the terminal
fn fill_out_buffer(
    row_buffer: &mut Vec<ColorSpec>,
    out_buffer: &mut Buffer,
    is_last_row: bool,
) -> ViuResult {
    let mut out_color;
    let mut out_char;
    let mut new_color;

    for c in row_buffer.iter() {
        // If a flush is needed it means that only one row with UPPER_HALF_BLOCK must be printed
        // because it is the last row, hence it contains only 1 pixel
        if is_last_row {
            new_color = ColorSpec::new();
            if let Some(bg) = c.bg() {
                new_color.set_fg(Some(*bg));
                out_char = UPPER_HALF_BLOCK;
            } else {
                execute!(out_buffer, MoveRight(1))?;
                continue;
            }
            out_color = &new_color;
        } else {
            match (c.fg(), c.bg()) {
                (None, None) => {
                    // completely transparent
                    execute!(out_buffer, MoveRight(1))?;
                    continue;
                }
                (Some(bottom), None) => {
                    // only top transparent
                    new_color = ColorSpec::new();
                    new_color.set_fg(Some(*bottom));
                    out_color = &new_color;
                    out_char = LOWER_HALF_BLOCK;
                }
                (None, Some(top)) => {
                    // only bottom transparent
                    new_color = ColorSpec::new();
                    new_color.set_fg(Some(*top));
                    out_color = &new_color;
                    out_char = UPPER_HALF_BLOCK;
                }
                (Some(_top), Some(_bottom)) => {
                    // both parts have a color
                    out_color = c;
                    out_char = LOWER_HALF_BLOCK;
                }
            }
        }
        out_buffer.set_color(out_color)?;
        write!(out_buffer, "{}", out_char)?;
    }

    clear_printer(out_buffer)?;
    writeln!(out_buffer)?;
    row_buffer.clear();

    Ok(())
}

fn is_pixel_transparent(pixel: (u32, u32, Rgba<u8>)) -> bool {
    let (_x, _y, data) = pixel;
    data[3] == 0
}

fn get_transparency_color(row: u32, col: u32, truecolor: bool) -> Color {
    //imitate the transparent chess board pattern
    let rgb = if row % 2 == col % 2 {
        (102, 102, 102)
    } else {
        (153, 153, 153)
    };
    if truecolor {
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    } else {
        Color::Ansi256(ansi256_from_rgb(rgb))
    }
}

fn get_color_from_pixel(pixel: (u32, u32, Rgba<u8>), truecolor: bool) -> Color {
    let (_x, _y, data) = pixel;
    let rgb = (data[0], data[1], data[2]);
    if truecolor {
        Color::Rgb(rgb.0, rgb.1, rgb.2)
    } else {
        Color::Ansi256(ansi256_from_rgb(rgb))
    }
}

fn clear_printer(out_buffer: &mut Buffer) -> ViuResult {
    let c = ColorSpec::new();
    out_buffer.set_color(&c).map_err(ViuError::IO)
}

// enum used to keep track where the current line of pixels processed should be displayed - as
// background or foreground color
#[derive(PartialEq)]
enum Mode {
    Top,
    Bottom,
}
