use anyhow::{bail, ensure, Context};
use clap::{Parser, Subcommand};
use core::fmt;
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use hex_hash::GitHexHash;
use jiff::Zoned;
use sha1::{Digest, Sha1};
use std::{
    env,
    ffi::CStr,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

mod hex_hash;

fn main() {
    if let Err(err) = try_main() {
        // We try to format the errors as git does.
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

            let object = ObjectReader::from_sha1(hash)?;
            match object {
                ObjectReader::Blob(mut reader) => {
                    io::copy(&mut reader, &mut io::stdout())
                        .context("piping object content to stdout")?;
                }
                ObjectReader::Tree(_) => bail!("we don't know how to print tree"),
            }
        }
        Command::HashObject { file, write } => {
            let sha1 = hash_object(&file, write)?;
            let sha1 = base16ct::lower::encode_string(&sha1);
            println!("{sha1}");
        }
        Command::LsTree { hash, name_only } => {
            print_tree(hash, name_only)?;
        }
        Command::WriteTree => {
            let working_dir = env::current_dir()?;
            let sha1 = write_tree(&working_dir)?;
            let sha1 = base16ct::lower::encode_string(&sha1);
            println!("{sha1}");
        }
        Command::CommitTree {
            tree_hash,
            parent_hash,
            message,
        } => {
            commit_tree(tree_hash, parent_hash, message)?;
        }
    };
    Ok(())
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
    CatFile {
        /// SHA-1 hash of the object in hexadecimal representation.
        #[arg(value_parser = parse_hash)]
        hash: GitHexHash,
        #[arg(short)]
        pretty_print: bool,
    },
    CommitTree {
        #[arg(value_parser = parse_hash)]
        tree_hash: GitHexHash,
        #[arg(short, long, value_parser = parse_hash)]
        parent_hash: GitHexHash,
        #[arg(short, long)]
        message: String,
    },
    /// Create blob object from file.
    HashObject {
        file: PathBuf,
        #[arg(short)]
        write: bool,
    },
    Init,
    LsTree {
        #[arg(value_parser = parse_hash)]
        hash: GitHexHash,
        #[arg(long)]
        name_only: bool,
    },
    WriteTree,
}

fn parse_hash(input: &str) -> anyhow::Result<GitHexHash> {
    GitHexHash::try_from(input)
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

enum Entry {
    Dir,
    File,
}

fn write_tree(dir: &Path) -> anyhow::Result<sha1::digest::Output<sha1::Sha1>> {
    let mut tree_entries = Vec::new();
    let mut names_len = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }

        if path.is_dir() {
            let sha1 = write_tree(&path)?;
            let file_name = entry.file_name().into_string().unwrap();
            // minus one because the mode of dir rectory are encoded will less byte.
            names_len += file_name.len() - 1;
            tree_entries.push((sha1, file_name, Entry::Dir))
        } else {
            // Each files are a blob object.
            let sha1 = hash_object(&entry.path(), true)?;
            let file_name = entry.file_name().into_string().unwrap();
            names_len += file_name.len();
            tree_entries.push((sha1, file_name, Entry::File))
        }
    }
    let tmp_path = env::temp_dir().join("tmp_tree");

    let tmp = fs::File::create(&tmp_path)?;
    // We don't use `BufWriter` here because, quite surprisingly, ZlibEncoder `Write` implementation already use a buffer.
    let mut hasher = ObjectHasher {
        hash: Sha1::new(),
        writer: ZlibEncoder::new(tmp, Compression::default()),
    };
    // 20 the sha1, 6 the mode, 1 the \0 and 1 the whitespace
    let entries_len = names_len + tree_entries.len() * (20 + 6 + 1 + 1);
    write!(hasher, "tree {entries_len}\0")?;
    tree_entries.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    for (sha1, file_name, kind) in tree_entries {
        match kind {
            // By observing git, the leading 0 displayed for dir mode is note encoded.
            Entry::Dir => write!(hasher, "40000")?,
            Entry::File => write!(hasher, "100644")?,
        }
        write!(hasher, " {file_name}\0")?;
        hasher.write_all(&sha1)?;
    }

    let _ = hasher.writer.finish()?;
    let hash = hasher.hash.finalize();
    let sha1 = base16ct::lower::encode_string(&hash);

    let (dir, rest) = sha1.split_at(2);
    let parent = PathBuf::from(".git/objects").join(dir);
    let object_path = parent.join(rest);
    fs::create_dir_all(&parent).context(format!("creating {parent:?}"))?;
    fs::rename(tmp_path, object_path)?;

    Ok(hash)
}

fn commit_tree(
    tree_hash: GitHexHash,
    parent_hash: GitHexHash,
    message: String,
) -> anyhow::Result<()> {
    let tmp_path = env::temp_dir().join("tmp_tree");

    let tmp = fs::File::create(&tmp_path)?;
    // We don't use `BufWriter` here because, quite surprisingly, ZlibEncoder `Write` implementation already use a buffer.
    let mut hasher = ObjectHasher {
        hash: Sha1::new(),
        writer: ZlibEncoder::new(tmp, Compression::default()),
    };
    const AUTHOR: &str = "John Doe";
    const EMAIL: &str = "johndoe@example.com";
    let now = jiff::Timestamp::now().as_second().to_string();
    // TODO: find a way to padd this value like git
    // 1732376559 +0100
    let _offset = Zoned::now().offset().to_string();

    // We pre-compute the length ahead of time so we don't have to write in a temporary buffer to compute the length.
    let length: usize = 5 // tree
        + 40
        + 1
        // parent
        + 7
        + 40
        + 1
        // author
        + 7
        + AUTHOR.as_bytes().len()
        + 2
        + EMAIL.as_bytes().len()
        + 2
        + now.len()
        + 6
        + 1
        // commiter
        + 9
        + AUTHOR.as_bytes().len()
        + 2
        + EMAIL.as_bytes().len()
        + 2
        + now.len()
        + 6
        + 1
        // new line
        + 1
        // message
        + message.as_bytes().len()
        // new line
        + 1;
    write!(hasher, "commit {length}\0")?;
    writeln!(hasher, "tree {tree_hash}")?;
    writeln!(hasher, "parent {parent_hash}")?;
    writeln!(hasher, "author {AUTHOR} <{EMAIL}> {now} +0000")?;
    writeln!(hasher, "commiter {AUTHOR} <{EMAIL}> {now} +0000")?;
    writeln!(hasher)?;
    writeln!(hasher, "{message}")?;
    let _ = hasher.writer.finish()?;

    let hash = hasher.hash.finalize();
    let sha1 = base16ct::lower::encode_string(&hash);

    let (dir, rest) = sha1.split_at(2);
    let parent = PathBuf::from(".git/objects").join(dir);
    let object_path = parent.join(rest);
    fs::create_dir_all(&parent).context(format!("creating {parent:?}"))?;
    fs::rename(tmp_path, object_path)?;

    println!("{sha1}");
    Ok(())
}

fn hash_object(file: &Path, write: bool) -> anyhow::Result<sha1::digest::Output<sha1::Sha1>> {
    // 1. Add the header
    // 2. Hash the object and compress it at the same time (so we need to read the whole file once). The compression is directly writen to a tmp file to avoid loading the whole file in memory
    // 3. Rename the temp file with the hash name
    Ok(if write {
        // Getting length ahead won't work with stdin. We also hope that the file don't get modified until we write it,
        // otherwise we could encode a bad length
        let file_len = fs::metadata(file).context("get {file:?} metadata")?.len();
        let mut opened_file = fs::File::open(file).context("open {file:?}")?;
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

        let (dir, rest) = sha1.split_at(2);
        let parent = PathBuf::from(".git/objects").join(dir);
        let object_path = parent.join(rest);
        fs::create_dir_all(&parent).context(format!("creating {parent:?}"))?;
        fs::rename(tmp_path, object_path)?;
        hash
    } else {
        // We don't want to read the whole file into memory to compute the len, so we use stat.
        let file_len = fs::metadata(file)?.len();
        let mut file = fs::File::open(file)?;
        let mut hasher = Sha1::new();
        write!(hasher, "blob {file_len}\0")?;
        io::copy(&mut file, &mut hasher)?;
        hasher.finalize()
    })
}

// Here there is not separator between the entries of the tree, they all start by a number but this could
// be melted with the sha1 bytes, so we can't have a "split on separator" approach. In other words the format is not self describing.
fn print_tree(hash: GitHexHash, name_only: bool) -> anyhow::Result<()> {
    let object = ObjectReader::from_sha1(hash)?;

    let ObjectReader::Tree(mut reader) = object else {
        bail!("not a tree object");
    };

    let mut mode_buf = Vec::with_capacity(6);
    let mut name_buf = Vec::new();
    let mut hash_buf = [0; 20];

    let mut stdout = io::stdout().lock();
    // Each entry is <mode> <name>\0 sha1
    loop {
        name_buf.clear();
        mode_buf.clear();
        let n = reader.read_until(b' ', &mut mode_buf)?;
        if n == 0 {
            break;
        }

        // Why they encode the mode in ASCII and not as an integer?
        let mode = std::str::from_utf8(&mode_buf[..mode_buf.len() - 1]).context("reading mode")?;

        let n = reader
            .read_until(0, &mut name_buf)
            .context("reading the header")?;
        let name = CStr::from_bytes_with_nul(&name_buf[..n])
            .context("reading name")?
            .to_str()?;

        reader.read_exact(&mut hash_buf).context("reading hash")?;
        let hex_hash = base16ct::lower::encode_string(&hash_buf);
        if name_only {
            writeln!(stdout, "{name}")?;
        } else {
            let object = ObjectReader::from_sha1(hex_hash.as_str().try_into()?)?;
            // In git on Linux (and windows for version >= V1.7.10) the CStr is encoded as UTF-8. However, by default
            // git ls-tree won't print the unicode symbole if not ASCII, it will escape the symbols in octal
            // representation.
            write!(stdout, "{mode:0>6} {object} {hex_hash}    ")?;
            for byte in name.as_bytes() {
                if byte.is_ascii() {
                    let char = char::from(*byte);
                    write!(stdout, "{char}")?;
                } else {
                    write!(stdout, "\\{byte:o}")?;
                }
            }

            writeln!(stdout)?;
        }
    }

    Ok(())
}

// Each object have an header
// <kind> <size>\0
// The size is the length of the content following the header.
enum ObjectReader<R> {
    Blob(R),
    Tree(R),
}

impl ObjectReader<()> {
    fn from_sha1(hash: GitHexHash) -> anyhow::Result<ObjectReader<impl BufRead>> {
        let object = hash.to_path();
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
            "blob" => ObjectReader::Blob(object),
            "tree" => ObjectReader::Tree(object),
            _ => bail!("unknown object kind: {kind}"),
        })
    }
}

impl<R> fmt::Display for ObjectReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectReader::Blob(_) => write!(f, "blob"),
            ObjectReader::Tree(_) => write!(f, "tree"),
        }
    }
}
