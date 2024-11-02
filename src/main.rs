use anyhow::{bail, ensure, Context};
use clap::{Parser, Subcommand};
use core::fmt;
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
            println!("Initialized git directory");
        }
        Command::CatFile { hash, pretty_print } => {
            ensure!(
                pretty_print,
                "We only handle the pretty print option -p for now"
            );

            let object = Object::from_sha1(&hash)?;
            match object {
                Object::Blob(mut reader) => {
                    io::copy(&mut reader, &mut io::stdout())
                        .context("piping object content to stdout")?;
                }
                Object::Tree(_) => bail!("we don't know how to print tree"),
            }
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
        Command::LsTree { hash, name_only } => {
            ensure!(
                hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                "Not a valid object name {hash}"
            );
            parse_tree(&hash, name_only)?;
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

// Here there is not separator between the entries of the tree, they all start by a number but this could
// be melt with the sha1 bytes, so we can't have a "split" approache. In other words the format is not self describing.
fn parse_tree(hash: &str, name_only: bool) -> anyhow::Result<()> {
    let object = Object::from_sha1(hash)?;

    let Object::Tree(mut reader) = object else {
        bail!("not a tree object");
    };

    let mut mode_buf = Vec::with_capacity(6);
    let mut name_buf = Vec::new();
    let mut hash_buf = [0; 20];

    let mut stdout = io::stdout().lock();
    loop {
        name_buf.clear();
        mode_buf.clear();
        let n = reader.read_until(b' ', &mut mode_buf)?;
        if n == 0 {
            break;
        }

        // Why they encode the mode in ASCII and not as an integer?
        let mode = std::str::from_utf8(&mode_buf[..mode_buf.len() - 1])?;

        let n = reader
            .read_until(0, &mut name_buf)
            .context("reading the header")?;
        let name = CStr::from_bytes_with_nul(&name_buf[..n])?.to_str()?;

        reader.read_exact(&mut hash_buf)?;
        let hex_hash = base16ct::lower::encode_string(&hash_buf);
        if name_only {
            writeln!(stdout, "{name}")?;
        } else {
            let object = Object::from_sha1(&hex_hash)?;
            writeln!(stdout, "{mode:0>6} {object} {hex_hash}    {name}")?;
        }
    }

    Ok(())
}

// Each object have an header
// <kind> <size>\0
enum Object<R> {
    Blob(R),
    Tree(R),
}

impl Object<()> {
    fn from_sha1(hash: &str) -> anyhow::Result<Object<impl BufRead>> {
        // `hash` is the hex representation of 20 bytes so it size must be 40.
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
        let size = size.parse::<u64>().context("parsing the size")?;
        // Takes protects from zip bomb.
        let object = z_decoder.take(size);
        Ok(match kind {
            "blob" => Object::Blob(object),
            "tree" => Object::Tree(object),
            _ => bail!("unknown object kind: {kind}"),
        })
    }
}

impl<R> fmt::Display for Object<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Object::Blob(_) => write!(f, "blob"),
            Object::Tree(_) => write!(f, "tree"),
        }
    }
}

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
        #[arg(long)]
        name_only: bool,
    },
}
