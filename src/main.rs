use anyhow::{anyhow, ensure};
use clap::{Parser, Subcommand};
use flate2::read::ZlibDecoder;
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};

fn main() -> anyhow::Result<()> {
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
                "fatal: Not a valid object name {hash}"
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
    // CatFile2 {
    //     #[arg(short = 'c')]
    //     hash: [u8; 40],
    // },
}
