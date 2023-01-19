extern crate binary_rw;
mod output;
use std::{path::{PathBuf, Path}, collections::HashMap};

use crunch::{Item, Rotation, Rect};
use image::{RgbaImage, ImageBuffer, GenericImage, GenericImageView, Rgba};

use crate::error::PackerError;

use self::output::{save_output, JsonOutput, BinaryOutput, RonOutput};

#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct PackerConfig {
    pub name: String,
    pub output_path: PathBuf,
    pub output_type: OutputType,
    pub folders: Vec<PathBuf>,
    pub template_path: Option<PathBuf>,
    pub options: PackerConfigOptions
}

impl PackerConfig {
    pub fn from_json(path: PathBuf) -> anyhow::Result<PackerConfig> {
        let buffer = std::fs::read(path)?;
        let packer_atlas = serde_json::from_slice::<PackerConfig>(&buffer)?;
        Ok(packer_atlas)
    }

    pub fn from_ron(path: PathBuf) -> anyhow::Result<PackerConfig> {
        let buffer = std::fs::read_to_string(path)?;
        let packer_atlas = ron::from_str::<PackerConfig>(&buffer)?;
        Ok(packer_atlas)
    }
}

#[derive(serde::Deserialize, serde::Serialize, clap::ValueEnum, Clone)]
pub enum OutputType {
    Json,
    Binary,
    Ron,
    Template
}

#[derive(serde::Deserialize, serde::Serialize, Clone)]
pub struct PackerConfigOptions {
    max_size: usize,
    show_extension: bool,
    rotation: bool
}

impl Default for PackerConfigOptions {
    fn default() -> Self {
        PackerConfigOptions { 
            max_size: 1024,
            show_extension: true,
            rotation: false
        }
    }
}

#[derive(Default, serde::Serialize)]
struct PackerAtlas {
    sheet_path: PathBuf,
    frames: HashMap<String, TextureData>
}

impl PackerAtlas {
    fn add(&mut self, name: &str, x: u32, y: u32, width: u32, height: u32, rotated: bool) {
        self.frames.insert(name.into(), TextureData {
            x, y, width, height, rotated
        });
    }

    fn add_sheet_path(&mut self, path: &Path) {
        self.sheet_path = path.to_path_buf();
    }
}

#[derive(serde::Serialize)]
struct TextureData {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rotated: bool
}

struct ImageTexture {
    name: String,
    img: RgbaImage
}

impl ImageTexture {
    const fn new(name: String, img: RgbaImage) -> Self {
        ImageTexture {
            name, img
        }
    }
}

fn visit_dir(dir: PathBuf, collector: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if dir.is_dir() {
        let paths = std::fs::read_dir(dir)?;
        for path in paths {
            let path = path?.path();
            if path.is_dir() {
                visit_dir(path, collector)?;
            } else {
                collector.push(path);
            }
        }
    }
    Ok(())
}

fn get_extension_from_filename(filename: &Path) -> Option<&str> {
    filename
        .extension()
        .and_then(std::ffi::OsStr::to_str)
}

pub fn pack(config: PackerConfig) -> anyhow::Result<()> {
    let mut image_paths = Vec::new();

    for folder in config.folders.iter() {
        let mut paths = Vec::new();
        visit_dir(folder.to_path_buf(), &mut paths)?;
        image_paths.extend(paths);
    }

    let images: Vec<ImageTexture> = image_paths.iter().filter_map(|file| {
        let filename = file;
        let ext = get_extension_from_filename(filename);
        if ext != Some("png") {
            return None;
        }
        println!("Found {}",filename.display());
        let filename = if !config.options.show_extension {
            filename.with_extension("")
                .to_str().unwrap_or_default().to_owned()
        } else {
            filename.to_str().unwrap_or_default().to_owned()
        };
        let Ok(img) = image::open(file) else { return None };
        Some(ImageTexture::new(filename, img.to_rgba8()))
    }).collect();

    let items: Vec<Item<&ImageTexture>> = images.iter().enumerate().map(|(_, img)| {
        let rotation = match config.options.rotation {
            true => Rotation::Allowed,
            false => Rotation::None
        };
        Item::new(img, img.img.width() as usize, img.img.height() as usize, rotation)
    }).collect();

    if let Ok((w, h, mut packed)) = crunch::pack_into_po2(config.options.max_size, items) {
        let mut atlas_json = PackerAtlas::default();
        let mut atlas: RgbaImage = ImageBuffer::from_fn(
            w as u32, 
            h as u32, 
            |_, _| image::Rgba([0, 0, 0, 0])
        );

        // Pack all images
        for (rect, image_data) in packed.iter_mut() {
            let (x, y) = (rect.x as u32, rect.y as u32);
            let img = if rect.rotated {
                rotate_90(rect, image_data)
            } else {
                image_data.img.to_owned()
            };
            let (width, height) = (rect.w as u32, rect.h as u32);

            let view = img.view(0, 0, width, height);
            atlas.copy_from(&view, x, y)?;
            atlas_json.add(&image_data.name, x, y, rect.w as u32, rect.h as u32, rect.rotated);
        }
        
        let mut path = config.output_path.clone();
        path.push(&config.name);
        path.set_extension("png");
        atlas_json.add_sheet_path(&path);

        let mut file_path = config.output_path.clone();
        file_path.push(&config.name);


        if !config.output_path.is_dir() {
            std::fs::create_dir_all(&config.output_path)?;
        }

        atlas.save(path)?;
        match config.output_type {
            OutputType::Json => save_output::<JsonOutput>(file_path, atlas_json)?,
            OutputType::Binary => save_output::<BinaryOutput>(file_path, atlas_json)?,
            OutputType::Ron => save_output::<RonOutput>(file_path, atlas_json)?,
            OutputType::Template => {
                let Some(ref template_path) = config.template_path else { 
                    Err(PackerError::NoTemplateFile)?
                };
                let template = std::fs::read_to_string(template_path)?;
                let mut handlerbars = handlebars::Handlebars::new();
                handlerbars.set_strict_mode(true);
                handlerbars.register_template_string("t1", template)?;
                let extension = template_path
                    .extension()
                    .unwrap_or_else(|| std::ffi::OsStr::new(""));
                let template_path = file_path.with_extension(extension);
                let globals = TemplateGlobals {
                    atlas: atlas_json,
                    config: config.clone()
                };

                let compiled = handlerbars.render("t1", &globals)?;
                let compiled = compiled.replace('\\', "/");
                std::fs::write(template_path, compiled)?;
            }
        }
        Ok(())
    } else {
        Err(PackerError::FailedToPacked)?
    }
}

#[derive(serde::Serialize)]
struct TemplateGlobals {
    atlas: PackerAtlas,
    config: PackerConfig
}

fn rotate_90(rect: &Rect, image_data: &ImageTexture) 
    -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    if rect.rotated {
        image::imageops::rotate90(&image_data.img)
    } else {
        image_data.img.to_owned()
    }
}

