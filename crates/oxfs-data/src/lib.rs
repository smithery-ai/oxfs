use async_trait::async_trait;
use bytes::Bytes;
use opendal::Operator;

#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("backend: {0}")]
    Backend(String),
}

pub type DataResult<T> = std::result::Result<T, DataError>;

#[async_trait]
pub trait DataEngine: Send + Sync {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes>;
    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()>;
    async fn delete_slice(&self, slice_id: u64) -> DataResult<()>;
}

pub struct OpenDalDataEngine {
    op: Operator,
}

impl OpenDalDataEngine {
    pub fn new(op: Operator) -> Self {
        Self { op }
    }

    fn slice_key(slice_id: u64) -> String {
        let prefix = slice_id / 1000;
        format!("slices/{prefix:04}/{slice_id:012}")
    }
}

#[async_trait]
impl DataEngine for OpenDalDataEngine {
    async fn read_slice(&self, slice_id: u64) -> DataResult<Bytes> {
        let key = Self::slice_key(slice_id);
        let data = self.op.read(&key).await.map_err(|e| match e.kind() {
            opendal::ErrorKind::NotFound => DataError::NotFound(key),
            _ => DataError::Backend(e.to_string()),
        })?;
        Ok(data.to_bytes())
    }

    async fn write_slice(&self, slice_id: u64, data: Bytes) -> DataResult<()> {
        let key = Self::slice_key(slice_id);
        self.op
            .write(&key, data)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn delete_slice(&self, slice_id: u64) -> DataResult<()> {
        let key = Self::slice_key(slice_id);
        self.op
            .delete(&key)
            .await
            .map_err(|e| DataError::Backend(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::services::Fs;

    async fn setup() -> (OpenDalDataEngine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let builder = Fs::default().root(dir.path().to_str().unwrap());
        let op = Operator::new(builder).unwrap().finish();
        (OpenDalDataEngine::new(op), dir)
    }

    #[tokio::test]
    async fn write_and_read_slice() {
        let (engine, _dir) = setup().await;
        let data = Bytes::from("hello oxfs");
        engine.write_slice(1, data.clone()).await.unwrap();
        let read = engine.read_slice(1).await.unwrap();
        assert_eq!(read, data);
    }

    #[tokio::test]
    async fn read_missing_slice() {
        let (engine, _dir) = setup().await;
        let err = engine.read_slice(999).await;
        assert!(matches!(err, Err(DataError::NotFound(_))));
    }

    #[tokio::test]
    async fn delete_slice() {
        let (engine, _dir) = setup().await;
        engine.write_slice(2, Bytes::from("gone")).await.unwrap();
        engine.delete_slice(2).await.unwrap();
        assert!(matches!(engine.read_slice(2).await, Err(DataError::NotFound(_))));
    }
}
