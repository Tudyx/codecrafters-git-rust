use anyhow::{anyhow, bail, ensure, Context};
use clap::{Parser, Subcommand};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use sha1::{Digest, Sha1};
use std::{
    env,
    ffi::CStr,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
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
        Command::CatFile { hash, pretty_print } => {
            if !pretty_print {
                eprintln!("We only handle the pretty print option for now");
                return Ok(());
            }

            // `hash` is the hex represenation of 20 bytes so it size must be 40.
            ensure!(
                hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                "Not a valid object name {hash}"
            );

            let (dir, rest) = hash.split_at(2);
            let object = PathBuf::from(".git/objects").join(dir).join(rest);
            let object = fs::File::open(&object).context(format!("opening {object:?}"))?;
            let z_decoder = ZlibDecoder::new(object);
            let mut z_decoder = BufReader::new(z_decoder);
            let mut object = Vec::new();
            // blob <size>\0<content>
            let n = z_decoder
                .read_until(0, &mut object)
                .context("reading the header")?;
            let header = CStr::from_bytes_with_nul(&object[..n])?.to_str()?;
            let (kind, size) = header.split_once(' ').context("spliting the header")?;
            ensure!(kind == "blob", "we only know how to print blob");
            let size = size.parse::<u64>().context("parsing the size")?;

            // Takes protects from zip bomb.
            let mut blob = z_decoder.take(size);

            io::copy(&mut blob, &mut io::stdout()).context("piping object content to stdout")?;
        }
        Command::HashObject { file, write } => {
            // 1. Add the header
            // 2. Hash the object and compress it at the same time (so we need to read the whole file once)
            // 4. Write it to disk (to avoid loading the whole file in memory)
            // 5. Rename it with the hash name
            if write {
                // Getting length ahead won't work with stdin.
                let file_len = fs::metadata(&file).context("get {file:?} metadata")?.len();
                let mut opened_file = fs::File::open(&file).context("open {file:?}")?;
                let tmp_path = env::temp_dir().join("tempfile");

                let tmp = fs::File::create(&tmp_path)?;
                let archive = ZlibEncoder::new(tmp, Compression::default());
                let mut archive = ObjectHasher {
                    hash: Sha1::new(),
                    writer: archive,
                };
                write!(archive, "blob {}\0", file_len)?;
                io::copy(&mut opened_file, &mut archive)?;
                let _ = archive.writer.finish()?;
                let hash = archive.hash.finalize();
                let sha1 = base16ct::lower::encode_string(&hash);
                println!("{sha1}");

                let (dir, rest) = sha1.split_at(2);
                let parent = PathBuf::from(".git/objects").join(dir);
                let object_path = parent.join(rest);
                fs::create_dir_all(&parent).context(format!("creating {parent:?}"))?;
                fs::rename(tmp_path, object_path)?;
            } else {
                // We don't want to read the whole file into memory to compute the len, so we use stat.
                let file_len = fs::metadata(&file)?.len();
                let mut file = fs::File::open(&file)?;
                let mut hasher = Sha1::new();
                write!(hasher, "blob {file_len}\0")?;
                io::copy(&mut file, &mut hasher)?;
                let sha1 = hasher.finalize();
                let sha1 = base16ct::lower::encode_string(&sha1);
                println!("{sha1}");
            }
        }
        Command::LsTree { hash } => {
            ensure!(
                hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                "Not a valid object name {hash}"
            );
            parse_tree(&hash)?;
        }
    };
    Ok(())
}

struct ObjectHasher<W> {
    hash: Sha1,
    writer: W,
}

impl<W> Write for ObjectHasher<W>
where
    W: Write,
{
    // We only have to iterate through the value once to compute the hash
    // and compress the data.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.writer.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    // TODO: impl write_vectored
}

fn parse_tree(hash: &str) -> anyhow::Result<()> {
    let (dir, rest) = hash.split_at(2);
    let object = PathBuf::from(".git/objects").join(dir).join(rest);
    let object = fs::read(object)?;
    let mut z_decoder = ZlibDecoder::new(object.as_slice());
    let mut object = Vec::new();
    // For now we load the whole object in memory, hoping it wont't be to big.
    z_decoder.read_to_end(&mut object)?;
    println!("full object:");
    std::io::stdout().write_all(&object).unwrap();
    println!("\n-----------------");

    let mut object_bytes = object.iter();
    let whitespace = object_bytes
        .position(|byte| byte == &b' ')
        .ok_or_else(|| anyhow!("invalid object content"))?;
    let kind = &object[..whitespace];
    std::io::stdout().write_all(kind).unwrap();
    object_bytes
        .position(|&byte| byte == b'\0')
        .ok_or_else(|| anyhow!("invalid object content"))?;
    match kind {
        b"tree" => (),
        _ => bail!("not a tree object"),
    };

    let whitespace = object_bytes
        .position(|byte| byte == &b' ')
        .ok_or_else(|| anyhow!("invalid object content"))?;
    let mode = &object[..whitespace];
    std::io::stdout().write_all(mode).unwrap();

    let whitespace = object_bytes
        .position(|byte| byte == &b' ')
        .ok_or_else(|| anyhow!("invalid object content"))?;
    let mode = &object[..whitespace];
    std::io::stdout().write_all(mode).unwrap();
    // TODO: while stream peak and consume the object
    // while stream.n

    // tree <size>\0<mode> <name>\0<20_byte_sha><mode> <name>\0<20_byte_sha><mode> <name>\0<20_byte_sha>
    // Here there is not separator beween the entries of the tree, they all start by a number but this could
    // be melt with the sha1 bytes, so we can't have a "split" appraoche.
    // So we need to track where we at when reading the tree, so we will use a reader.

    // io::stdout().write_all(content)?;

    // for entry in conenet
    Ok(())
}

enum Object {
    Blob(Vec<u8>),
    Tree,
}

// impl Object {
//     fn from_disk(sha: String) -> anyhow::Result<Self> {
//         ensure!(
//             sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
//             "fatal: Not a valid object name {sha}"
//         );
//         let (dir, rest) = sha.split_at(2);
//         let object = PathBuf::from(".git/objects").join(dir).join(rest);
//         let object = fs::read(object)?;
//         let mut z_decoder = ZlibDecoder::new(object.as_slice());
//         let mut object = Vec::new();
//         z_decoder.read_to_end(&mut object)?;
//         // If split_once for slice would be stable it would be perfect
//         let null_character = object
//             .iter()
//             .position(|&byte| byte == b'\0')
//             .ok_or_else(|| anyhow!("invalid object content"))?;
//         let content = &object[null_character + 1..];
//         let whitespace = object[..null_character]
//             .iter()
//             .position(|&byte| byte == b'_')
//             .ok_or_else(|| anyhow!("invalid object content"))?;
//         let kind = &object[..whitespace];
//         Ok(match kind {
//             b"blob" => Object::Blob(object),
//             b"tree" => Object::Tree,
//             _ => bail!("Unsupported object kind {}", String::from_utf8_lossy(kind)),
//         })
//     }
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
        /// SHA-1 hash of the object in hexadecimal representation.
        hash: String,

        #[arg(short)]
        pretty_print: bool,
    },
    HashObject {
        file: PathBuf,
        #[arg(short)]
        write: bool,
    },
    LsTree {
        hash: String,
    },
}
