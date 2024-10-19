use anyhow::{anyhow, ensure};
use clap::{Parser, Subcommand};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use sha1::{Digest, Sha1};
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};

fn main() {
    if let Err(err) = try_main() {
        eprintln!("fatal: {err}");
    }
}

fn try_main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Init => {
            fs::create_dir(".git").unwrap();
            fs::create_dir(".git/objects").unwrap();
            fs::create_dir(".git/refs").unwrap();
            fs::write(".git/HEAD", "ref: refs/heads/main\n").unwrap();
            println!("Initialized git directory")
        }
        Command::CatFile { hash } => {
            // `hash` is the hex represenation of 20 bytes so it size must be 40.
            ensure!(
                hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                "Not a valid object name {hash}"
            );
            let (dir, rest) = hash.split_at(2);
            let object = PathBuf::from(".git/objects").join(dir).join(rest);
            let object = fs::read(object)?;
            let mut z_decoder = ZlibDecoder::new(object.as_slice());
            let mut object = Vec::new();
            z_decoder.read_to_end(&mut object)?;
            // If split_once for slice would be stable it would be perfect
            let separator_position = object
                .iter()
                .position(|&byte| byte == b'\0')
                .ok_or_else(|| anyhow!("invalid object content"))?;
            io::stdout().write_all(&object[separator_position + 1..])?;
        }
        Command::HashObject { file, write } => {
            let mut file_content = fs::read(file)?;
            let mut object = format!("blob {}\0", file_content.len()).into_bytes();
            object.append(&mut file_content);

            let mut hasher = Sha1::new();
            hasher.update(&object);
            let sha1 = hasher.finalize();
            let sha1 = base16ct::lower::encode_string(&sha1);
            println!("{sha1}");

            if write {
                let (dir, rest) = sha1.split_at(2);
                let parent = PathBuf::from(".git/objects").join(dir);
                let object_path = parent.join(rest);

                let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&object)?;
                let compressed = encoder.finish()?;

                fs::create_dir_all(&parent)?;
                fs::write(object_path, compressed)?;
            }
        }
    };
    Ok(())
}

// enum Object<'de> {
//     Blob(&'de [u8]),
// }

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Init,
    // CatFile { hash: Box<[u8; 40]> },
    CatFile {
        /// Pretty print the object
        #[arg(short = 'p')]
        hash: String,
    },
    HashObject {
        file: PathBuf,
        #[arg(short)]
        write: bool,
    },
}
