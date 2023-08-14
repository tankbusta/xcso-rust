use std::io::{Error, ErrorKind, Write};
use std::fs::File;
use std::env;
use std::io;
use std::ffi::OsString;
use std::io::{Read, Seek};
use std::path::Path;
use byteorder::{WriteBytesExt, LittleEndian};
use lz4_flex::frame::{FrameInfo, BlockMode};

use console::{style, Emoji};
use indicatif::ProgressBar;

static CISO_MAGIC: u32 = 0x4F534943; // CISO
static CISO_HEADER_SIZE: u32 = 0x18; // 24
static CISO_BLOCK_SIZE: usize = 0x800; // 2048

static CLIP: Emoji<'_, '_> = Emoji("🔗  ", "");

#[derive(Copy, Clone)]
struct CsoImage {
    version: u8,
    align: u8,
    total_bytes: u64,
    total_blocks: usize,
}

fn get_filename_from_path(fp: &String) -> String {
    let path = Path::new(fp);
    return String::from(
        path.file_name()
            .unwrap_or(&OsString::from(""))
            .to_str()
            .unwrap_or(""),
    );
}

fn is_iso(fp: &String) -> bool {
    let path = Path::new(fp);
    let ext = String::from(
        path.extension()
            .unwrap_or(&OsString::from(""))
            .to_str()
            .unwrap_or(""),
    );

    match ext.as_str() {
        "xiso"|"iso" => true,
        _ => false,
    }
}

fn get_image_offset(f: &mut File) -> Result<u32, io::Error> {
    let mut buf: Vec<u8> = vec![0; 20];
    let xbox_media_header: Vec<u8> = b"MICROSOFT*XBOX*MEDIA".to_vec();

    // Check for redump
    _ = f.seek(io::SeekFrom::Start(0x18310000));
    _ = f.read_exact(&mut buf);

    if xbox_media_header == buf {
        return Ok(0x18300000);
    }

    // Check for XDVDFS
    _ = f.seek(io::SeekFrom::Start(0x10000));
    _ = f.read_exact(&mut buf);
    if xbox_media_header == buf {
        return Ok(0x0);
    }

    return Err(Error::new(ErrorKind::Other, "could not get image offset"));
}

fn pad_file(f: &mut File) -> Result<(), io::Error> {
    let end = f.seek(io::SeekFrom::End(0))?;
    let pad_size = end & 0x3FF;

    let buf: Vec<u8> = vec![0; 0x400-pad_size as usize];

    _ = f.write(&buf)?;
    return Ok(());
}

fn get_cso_info(f: &mut File) -> Result<CsoImage, io::Error> {
    let image_offset = get_image_offset(f)?;
    let fmetadata = f.metadata()?;

    let byte_len: u64 = fmetadata.len() - image_offset as u64;
    let blocks: usize = (byte_len as usize/ CISO_BLOCK_SIZE) as usize;
    return Ok(CsoImage {
        version: 2,
        align: 2,
        total_bytes: byte_len,
        total_blocks: blocks,
    });
}

fn write_cso_info(f: &mut File, img_data: CsoImage) -> Result<(), Error> {
    let mut buf: Vec<u8> = Vec::new();
    buf.write(&CISO_MAGIC.to_le_bytes()).unwrap();
    buf.write(&CISO_HEADER_SIZE.to_le_bytes()).unwrap();
    buf.write(&img_data.total_bytes.to_le_bytes()).unwrap();

    let block_size = CISO_BLOCK_SIZE as u32;
    buf.write(&block_size.to_le_bytes()).unwrap();

    buf.write_u8(img_data.version).unwrap();
    buf.write_u8(img_data.align).unwrap();

    let pad: u16 = 0;
    buf.write(&pad.to_le_bytes()).unwrap();

    assert_eq!(CISO_HEADER_SIZE, buf.len() as u32);
    f.write_all(&buf)
}

fn write_block_index(f: &mut File, blocks: &Vec<u32>) -> Result<u64, Error> {
    for block in blocks.iter() {
        match f.write_u32::<LittleEndian>(*block) {
            Ok(_) => (),
            Err(e) => return Err(e),
        }
    }

    // Get the current position
    return f.seek(io::SeekFrom::Current(0));
}

fn compress_block(block: Vec<u8>) -> Result<Vec<u8>, Error> {
    let uncompressed_size = block.len();
    let compressed_data: Vec<u8> = Vec::new();

    let mut frame_info = FrameInfo::new();
    frame_info.content_size = Some(uncompressed_size as u64);
    frame_info.block_checksums = false;
    frame_info.content_checksum = false;
    frame_info.block_mode = BlockMode::Independent;

    let mut enc = lz4_flex::frame::FrameEncoder::with_frame_info(frame_info, compressed_data);

    std::io::Write::write_all(&mut enc, &block)?;
    Ok(enc.finish()?)
}

fn compress_iso(fp: &String) -> Result<String, io::Error> {
    let fd_result = File::open(fp);
    let mut iso_file = match fd_result {
        Ok(file) => file,
        Err(e) => return Err(e),
    };

    let image_details = get_cso_info(&mut iso_file)?;

    // TODO: Split files
    let dest_fp = fp.to_owned() + ".1.cso";
    let mut dest_f1: File = File::create(dest_fp.clone())?;

    // Write the CSO header
    write_cso_info(&mut dest_f1, image_details)?;
    
    // Followed by a placeholder block index
    let block_size = usize::try_from(image_details.total_blocks).unwrap();
    let mut block_index = vec![0; block_size+1];
    let mut write_pos = write_block_index(&mut dest_f1, &block_index)?;

    let align_b = 1 << image_details.align;
    let align_m = align_b - 1;
    let alignment_buffer: Vec<u32> = vec![0; 64];

    // Holds the block size
    let mut blockbuf = vec![0; CISO_BLOCK_SIZE];
    let pb = ProgressBar::new(image_details.total_blocks as u64);

    for block in 0..image_details.total_blocks {
        // Check if we need to split the ISO (due to FATX limitations)
		// TODO: Determine a better value for this.
        if write_pos > 0xFFBF6000 {
            panic!("TODO: Split support")
        }

        let mut align: u64 = write_pos & align_m;
        if align > 0 {
            align = align_b - align;
            for b in alignment_buffer.iter().take(align as usize) {
                _ = dest_f1.write_u32::<LittleEndian>(*b)
            }
            write_pos += align;
        }

        block_index[block] = write_pos as u32 >> image_details.align as u32;
        let read = iso_file.read(&mut blockbuf[..])?;
        let compressed = compress_block(blockbuf[..read].to_vec())?;

        // If the compressed size is greater than the original, prefer the original
        if compressed.len() + 12 > read {
            write_pos += read as u64;
            dest_f1.write(&blockbuf[..read])?;
        } else {
            block_index[block] |= 0x80000000;
            write_pos += compressed.len() as u64;
            dest_f1.write(&compressed)?;      
        }

        pb.inc(1);
    }

    // end for block
	// last position (total size)
	// NOTE: We don't actually need this, but we're keeping it for legacy reasons.
    let last = block_index.len()-1;
    block_index[last] = write_pos as u32 >> image_details.align as u32;

    dest_f1.seek(io::SeekFrom::Start(CISO_HEADER_SIZE as u64))?;
    write_block_index(&mut dest_f1, &block_index)?;

    pad_file(&mut dest_f1)?;
    pb.finish_and_clear();

    return Ok(dest_fp);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() == 1 {
        println!("{} usage: <isos to convert>", get_filename_from_path(&args[0]));
        return;
    }

    for (i, fname) in args.iter().skip(1).enumerate() {
        if !is_iso(fname) {
            continue;
        }
        
        let fancy_file: String = format!("[{}/{}]", i+1, args.len()-1);
        println!(
            "{} {}Converting image {}...",
            style(fancy_file.clone()).bold().dim(),
            CLIP,
            fname,
        );

        match compress_iso(fname) {
            Ok(fp) => {
                println!(
                    "{} {}Converted image {}!",
                    style(fancy_file).bold().dim(),
                    CLIP,
                    fp,
                );
                continue;
            },
            Err(e) => {
                eprintln!("Error converting {}: {}", fname, e);
                continue;
            },
        };
    }
}
