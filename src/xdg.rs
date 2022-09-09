//! Enumerate installed applications.

use std::collections::HashMap;
use std::path::PathBuf;
use std::{fs, io, slice};

use image::error::ImageError;
use image::imageops::FilterType;
use image::io::Reader as ImageReader;
use xdg::BaseDirectories;

use crate::svg::{self, Svg};

/// Icon lookup paths in reverse order.
const ICON_PATHS: &[(&str, &str)] = &[
    ("/usr/share/icons/hicolor/32x32/apps/", "png"),
    ("/usr/share/icons/hicolor/64x64/apps/", "png"),
    ("/usr/share/icons/hicolor/256x256/apps/", "png"),
    ("/usr/share/icons/hicolor/scalable/apps/", "svg"),
    ("/usr/share/icons/hicolor/128x128/apps/", "png"),
    ("/usr/share/pixmaps/", "svg"),
    ("/usr/share/pixmaps/", "png"),
];

/// Desired size for PNG icons at a scale factor of 1.
const ICON_SIZE: u32 = 64;

#[derive(Debug)]
pub struct DesktopEntries {
    entries: Vec<DesktopEntry>,
    loader: IconLoader,
    scale_factor: u32,
}

impl DesktopEntries {
    /// Get icons for all installed applications.
    pub fn new(scale_factor: u32) -> Self {
        // Get all directories containing desktop files.
        let base_dirs = BaseDirectories::new().expect("Unable to get XDG base directories");
        let dirs = base_dirs.get_data_dirs();

        // Initialize icon loader.
        let loader = IconLoader::new();

        let mut desktop_entries = DesktopEntries { scale_factor, loader, entries: Vec::new() };

        // Find all desktop files in these directories, then look for their icons and
        // executables.
        let icon_size = desktop_entries.icon_size();
        for dir_entry in dirs.iter().flat_map(|d| fs::read_dir(d.join("applications")).ok()) {
            for desktop_file in dir_entry
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".desktop"))
                .flat_map(|entry| fs::read_to_string(entry.path()).ok())
            {
                let mut icon = None;
                let mut exec = None;
                let mut name = None;

                for line in desktop_file.lines() {
                    if let Some(value) = line.strip_prefix("Name=") {
                        name = Some(value.to_owned());
                    } else if let Some(value) = line.strip_prefix("Icon=") {
                        icon = desktop_entries.loader.load(value, icon_size).ok();
                    } else if let Some(value) = line.strip_prefix("Exec=") {
                        exec = value.split(' ').next().map(String::from);
                    }

                    if icon.is_some() && exec.is_some() && name.is_some() {
                        break;
                    }
                }

                if let Some(((name, icon), exec)) = name.zip(icon).zip(exec) {
                    desktop_entries.entries.push(DesktopEntry { icon, name, exec });
                }
            }
        }

        desktop_entries
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: u32) {
        // Avoid re-rasterization of icons when factor didn't change.
        if self.scale_factor == scale_factor {
            return;
        }
        self.scale_factor = scale_factor;

        // Update every icon.
        let icon_size = self.icon_size();
        for entry in &mut self.entries {
            if let Ok(icon) = self.loader.load(&entry.icon.name, icon_size) {
                entry.icon = icon;
            }
        }
    }

    /// Desktop icon size.
    pub fn icon_size(&self) -> u32 {
        ICON_SIZE * self.scale_factor
    }

    /// Create an iterator over all applications.
    pub fn iter(&self) -> slice::Iter<'_, DesktopEntry> {
        self.entries.iter()
    }

    /// Get the desktop entry at the specified index.
    pub fn get(&self, index: usize) -> Option<&DesktopEntry> {
        self.entries.get(index)
    }

    /// Number of installed applications.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Desktop entry information.
#[derive(Debug)]
pub struct DesktopEntry {
    pub icon: Icon,
    pub name: String,
    pub exec: String,
}

/// Rendered icon.
#[derive(Debug, Clone)]
pub struct Icon {
    pub data: Vec<u8>,
    pub width: usize,
    name: String,
}

/// Simple loader for app icons.
#[derive(Debug)]
struct IconLoader {
    icons: HashMap<String, PathBuf>,
}

impl IconLoader {
    /// Initialize the icon loader.
    ///
    /// This will check all paths for available icons and store them for cheap
    /// lookup.
    fn new() -> Self {
        let mut icons = HashMap::new();

        // Check all paths for icons.
        //
        // Since the `ICON_PATHS` is in reverse order of our priority, we can just
        // insert every new icon into `icons` and it will correctly return the
        // closest match.
        for (path, extension) in ICON_PATHS {
            let mut read_dir = fs::read_dir(path).ok();
            let entries = read_dir.iter_mut().flatten().flatten();
            let files = entries.filter(|e| e.file_type().map_or(false, |e| e.is_file()));

            // Iterate over all files in the directory.
            for file in files {
                let file_name = file.file_name().to_string_lossy().to_string();

                // Store icon paths with the correct extension.
                let name = file_name.rsplit_once('.').filter(|(_, ext)| ext == extension);
                if let Some((name, _)) = name {
                    let _ = icons.insert(name.to_owned(), file.path());
                }
            }
        }

        Self { icons }
    }

    /// Load image file as RGBA buffer.
    fn load(&self, icon: &str, size: u32) -> Result<Icon, Error> {
        let name = icon.into();

        let path = self.icons.get(icon).ok_or(Error::NotFound)?;
        let path_str = path.to_string_lossy();

        match &path_str[path_str.len() - 4..] {
            ".png" => {
                let mut image = ImageReader::open(path)?.decode()?;

                // Resize buffer if needed.
                if image.width() != size && image.height() != size {
                    image = image.resize(size, size, FilterType::CatmullRom);
                }

                // Premultiply alpha.
                let width = image.width() as usize;
                let mut data = image.into_bytes();
                for chunk in data.chunks_mut(4) {
                    chunk[0] = (chunk[0] as f32 * chunk[3] as f32 / 255.).round() as u8;
                    chunk[1] = (chunk[1] as f32 * chunk[3] as f32 / 255.).round() as u8;
                    chunk[2] = (chunk[2] as f32 * chunk[3] as f32 / 255.).round() as u8;
                }

                Ok(Icon { data, width, name })
            },
            ".svg" => {
                let svg = Svg::from_path(path, size)?;
                Ok(Icon { data: svg.data, width: svg.width, name })
            },
            _ => unreachable!(),
        }
    }
}

/// Icon loading error.
#[derive(Debug)]
pub enum Error {
    Image(ImageError),
    Svg(svg::Error),
    Io(io::Error),
    NotFound,
}

impl From<ImageError> for Error {
    fn from(error: ImageError) -> Self {
        Self::Image(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<svg::Error> for Error {
    fn from(error: svg::Error) -> Self {
        Self::Svg(error)
    }
}
