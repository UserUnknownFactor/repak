use super::{ext::ReadExt, Compression, Version};
use byteorder::{ReadBytesExt, LE};
use std::io;

#[derive(Debug)]
pub struct Block {
    pub start: u64,
    pub end: u64,
}

impl Block {
    pub fn new<R: io::Read>(reader: &mut R) -> Result<Self, super::Error> {
        Ok(Self {
            start: reader.read_u64::<LE>()?,
            end: reader.read_u64::<LE>()?,
        })
    }
}

#[derive(Debug)]
pub struct Entry {
    pub offset: u64,
    pub compressed: u64,
    pub uncompressed: u64,
    pub compression: Compression,
    pub timestamp: Option<u64>,
    pub hash: [u8; 20],
    pub blocks: Option<Vec<Block>>,
    pub encrypted: bool,
    pub block_uncompressed: Option<u32>,
}

impl Entry {
    pub fn new<R: io::Read>(reader: &mut R, version: super::Version) -> Result<Self, super::Error> {
        // since i need the compression flags, i have to store these as variables which is mildly annoying
        let offset = reader.read_u64::<LE>()?;
        let compressed = reader.read_u64::<LE>()?;
        let uncompressed = reader.read_u64::<LE>()?;
        let compression = match reader.read_u32::<LE>()? {
            0x01 | 0x10 | 0x20 => Compression::Zlib,
            _ => Compression::None,
        };
        Ok(Self {
            offset,
            compressed,
            uncompressed,
            compression,
            timestamp: match version == Version::Initial {
                true => Some(reader.read_u64::<LE>()?),
                false => None,
            },
            hash: reader.read_guid()?,
            blocks: match version >= Version::CompressionEncryption
                && compression != Compression::None
            {
                true => Some(reader.read_array(Block::new)?),
                false => None,
            },
            encrypted: version >= Version::CompressionEncryption && reader.read_bool()?,
            block_uncompressed: match version >= Version::CompressionEncryption {
                true => Some(reader.read_u32::<LE>()?),
                false => None,
            },
        })
    }

    pub fn read<R: io::Read + io::Seek, W: io::Write>(
        &self,
        reader: &mut R,
        version: super::Version,
        key: Option<&aes::Aes256Dec>,
        buf: &mut W,
    ) -> Result<(), super::Error> {
        reader.seek(io::SeekFrom::Start(self.offset))?;
        Entry::new(reader, version)?;
        let data_offset = reader.stream_position()?;
        let mut data = reader.read_len(match self.encrypted {
            // add alignment (aes block size: 16) then zero out alignment bits
            true => (self.compressed + 15) & !17,
            false => self.compressed,
        } as usize)?;
        if self.encrypted {
            let Some(key) = key else {
                return Err(super::Error::Encrypted);
            };
            use aes::cipher::BlockDecrypt;
            for block in data.chunks_mut(16) {
                key.decrypt_block(aes::Block::from_mut_slice(block))
            }
            data.truncate(self.compressed as usize);
        }
        macro_rules! decompress {
            ($decompressor: ty) => {
                match &self.blocks {
                    Some(blocks) => {
                        for block in blocks {
                            io::copy(
                                &mut <$decompressor>::new(
                                    &data[match version >= Version::RelativeChunkOffsets {
                                        true => {
                                            (block.start - (data_offset - self.offset)) as usize
                                                ..(block.end - (data_offset - self.offset)) as usize
                                        }
                                        false => {
                                            (block.start - data_offset) as usize
                                                ..(block.end - data_offset) as usize
                                        }
                                    }],
                                ),
                                buf,
                            )?;
                        }
                    }
                    None => {
                        io::copy(&mut flate2::read::ZlibDecoder::new(data.as_slice()), buf)?;
                    }
                }
            };
        }
        match self.compression {
            Compression::None => buf.write_all(&data)?,
            Compression::Zlib => decompress!(flate2::read::ZlibDecoder<&[u8]>),
            Compression::Gzip => decompress!(flate2::read::GzDecoder<&[u8]>),
            Compression::Oodle => todo!(),
        }
        buf.flush()?;
        Ok(())
    }
}
