// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{AnalyticsWriter, FileFormat, FileType};
use crate::{ParquetSchema, ParquetValue};
use anyhow::{anyhow, Result};
use arrow_array::{ArrayRef, RecordBatch, StringArray, UInt64Array};
use serde::Serialize;
use std::fs::File;
use std::fs::{create_dir_all, remove_file};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use sui_types::base_types::EpochId;

use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use sui_storage::object_store::util::path_to_filesystem;

// Save table entries to parquet files.
pub(crate) struct ParquetWriter {
    root_dir_path: PathBuf,
    file_type: FileType,
    epoch: EpochId,
    checkpoint_range: Range<u64>,
    data: Vec<Vec<ParquetValue>>,
}

impl ParquetWriter {
    pub(crate) fn new(
        root_dir_path: &Path,
        file_type: FileType,
        start_checkpoint_seq_num: u64,
    ) -> Result<Self> {
        let checkpoint_range = start_checkpoint_seq_num..u64::MAX;
        Ok(Self {
            root_dir_path: root_dir_path.to_path_buf(),
            file_type,
            epoch: 0,
            checkpoint_range,
            data: vec![],
        })
    }

    fn file(&self) -> Result<File> {
        let file_path = path_to_filesystem(
            self.root_dir_path.clone(),
            &self.file_type.file_path(
                FileFormat::PARQUET,
                self.epoch,
                self.checkpoint_range.clone(),
            ),
        )?;
        create_dir_all(file_path.parent().ok_or(anyhow!("Bad directory path"))?)?;
        if file_path.exists() {
            remove_file(&file_path)?;
        }
        Ok(File::create(&file_path)?)
    }
}

impl<S: Serialize + ParquetSchema> AnalyticsWriter<S> for ParquetWriter {
    fn file_format(&self) -> Result<FileFormat> {
        Ok(FileFormat::PARQUET)
    }

    fn write(&mut self, rows: &[S]) -> Result<()> {
        for row in rows {
            for idx in 0..S::schema().len() {
                if idx == self.data.len() {
                    self.data.push(vec![]);
                }
                self.data[idx].push(row.get_column(idx));
            }
        }
        Ok(())
    }

    fn flush(&mut self, end_checkpoint_seq_num: u64) -> Result<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        self.checkpoint_range.end = end_checkpoint_seq_num;
        let data = std::mem::take(&mut self.data);
        let mut batch_data = vec![];
        for column in data {
            match &column[0] {
                ParquetValue::Int(_) => {
                    let array = UInt64Array::from(
                        column
                            .into_iter()
                            .flat_map(|value| match value {
                                ParquetValue::Int(value) => Some(value),
                                _ => None,
                            })
                            .collect::<Vec<_>>(),
                    );
                    batch_data.push(Arc::new(array) as ArrayRef);
                }
                ParquetValue::Str(_) => {
                    let array = StringArray::from(
                        column
                            .into_iter()
                            .flat_map(|value| match value {
                                ParquetValue::Str(value) => Some(value),
                                _ => None,
                            })
                            .collect::<Vec<_>>(),
                    );
                    batch_data.push(Arc::new(array) as ArrayRef);
                }
            };
        }
        let batch = RecordBatch::try_from_iter(S::schema().iter().zip(batch_data.into_iter()))?;

        let properties = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();

        let mut writer = ArrowWriter::try_new(self.file()?, batch.schema(), Some(properties))?;
        writer.write(&batch)?;
        writer.close()?;
        self.checkpoint_range.start = self.checkpoint_range.end + 1;
        Ok(())
    }

    fn reset(&mut self, epoch_num: EpochId, start_checkpoint_seq_num: u64) -> Result<()> {
        self.checkpoint_range.start = start_checkpoint_seq_num;
        self.checkpoint_range.end = u64::MAX;
        self.epoch = epoch_num;
        self.data = vec![];
        Ok(())
    }
}
