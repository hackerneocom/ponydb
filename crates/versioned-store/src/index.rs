use super::*;

/// Storage location shared by all nodes written in an index operation.
#[derive(Clone, Copy, Debug)]
pub struct IndexWriteTarget<'a> {
    pub incarnation: &'a str,
    pub epoch: u64,
    pub category: &'a str,
}

impl<S: BlobStore + 'static> DatabaseStore<S> {
    pub async fn index_get(&self, mut root: Option<ObjectRef>, key: &str) -> Result<Option<Value>> {
        valid_key(key)?;
        let mut previous_len = None;
        for _ in 0..=64 {
            let Some(reference) = root else {
                return Ok(None);
            };
            let node = self.get_index_node(&reference).await?;
            if previous_len.is_some_and(|n| node.prefix.len() <= n) {
                return Err(Error::Corrupt);
            }
            if !key.starts_with(&node.prefix) {
                return Ok(None);
            }
            if key == node.prefix {
                return Ok(node.value.clone());
            }
            previous_len = Some(node.prefix.len());
            let c = key.as_bytes()[node.prefix.len()] as char;
            root = node.children.get(&c).cloned();
        }
        Err(Error::Corrupt)
    }
    pub async fn index_insert(
        &self,
        root: Option<ObjectRef>,
        key: &str,
        value: Value,
        target: IndexWriteTarget<'_>,
        ctx: &RequestContext,
    ) -> Result<ObjectRef> {
        self.index_update(
            root,
            BTreeMap::from([(key.into(), Some(value))]),
            target,
            ctx,
        )
        .await?
        .ok_or(Error::Corrupt)
    }
    /// Bounded traversal page; cursor is the last consumed logical key.
    pub async fn index_list(
        &self,
        root: Option<ObjectRef>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Value)>> {
        if limit == 0 || limit > 1000 {
            return Err(Error::Limit);
        }
        let mut stack = Vec::new();
        if let Some(root) = root {
            stack.push((root, None));
        }
        let mut out = Vec::new();
        let mut visits = 0;
        while let Some((reference, parent_len)) = stack.pop() {
            visits += 1;
            if visits > 100000 {
                return Err(Error::Limit);
            }
            let node = self.get_index_node(&reference).await?;
            if parent_len.is_some_and(|n| node.prefix.len() <= n) {
                return Err(Error::Corrupt);
            }
            if after.is_some_and(|a| node.prefix.as_str() < a && !a.starts_with(&node.prefix)) {
                continue;
            }
            if let Some(value) = &node.value {
                if after.is_none_or(|a| node.prefix.as_str() > a) {
                    out.push((node.prefix.clone(), value.clone()));
                    if out.len() == limit {
                        break;
                    }
                }
            }
            for child in node.children.values().rev() {
                stack.push((child.clone(), Some(node.prefix.len())));
            }
        }
        Ok(out)
    }
}
fn valid_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        Err(Error::Corrupt)
    } else {
        Ok(())
    }
}

impl<S: BlobStore + 'static> DatabaseStore<S> {
    pub async fn index_remove(
        &self,
        root: Option<ObjectRef>,
        key: &str,
        target: IndexWriteTarget<'_>,
        ctx: &RequestContext,
    ) -> Result<Option<ObjectRef>> {
        self.index_update(root, BTreeMap::from([(key.into(), None)]), target, ctx)
            .await
    }
}

// Only paths touched by this batch are materialized. Unchanged subtrees retain
// their original references; no intermediate roots are uploaded.
enum Pending {
    Stored(ObjectRef),
    Dirty(PendingNode),
}
struct PendingNode {
    prefix: String,
    value: Option<Value>,
    children: BTreeMap<char, Pending>,
}
impl<S: BlobStore + 'static> DatabaseStore<S> {
    /// Apply inserts (Some) and removals (None), then persist each changed node once.
    pub async fn index_update(
        &self,
        root: Option<ObjectRef>,
        updates: BTreeMap<String, Option<Value>>,
        target: IndexWriteTarget<'_>,
        ctx: &RequestContext,
    ) -> Result<Option<ObjectRef>> {
        let IndexWriteTarget {
            incarnation: inc,
            epoch,
            category,
        } = target;
        // Reject invalid path inputs before loading any existing index nodes.
        IncarnationId::new(inc).map_err(|_| Error::Corrupt)?;
        RelativeKey::new(category).map_err(|_| Error::Corrupt)?;
        if epoch == 0 {
            return Err(Error::Corrupt);
        }
        for key in updates.keys() {
            valid_key(key)?;
        }
        let mut pending = root.map(Pending::Stored);
        for (key, value) in updates {
            pending = self.edit(pending, &key, value, ctx, 0).await?;
        }
        match pending {
            Some(node) => Ok(Some(self.flush(node, inc, epoch, category, ctx).await?)),
            None => Ok(None),
        }
    }
    fn edit<'a>(
        &'a self,
        root: Option<Pending>,
        key: &'a str,
        value: Option<Value>,
        ctx: &'a RequestContext,
        depth: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Pending>>> + Send + 'a>> {
        Box::pin(async move {
            ctx.check()?;
            if depth > 64 {
                return Err(Error::Corrupt);
            }
            let Some(root) = root else {
                return Ok(value.map(|value| {
                    Pending::Dirty(PendingNode {
                        prefix: key.into(),
                        value: Some(value),
                        children: BTreeMap::new(),
                    })
                }));
            };
            let (mut node, original) = match root {
                Pending::Stored(reference) => {
                    let node = self.get_index_node(&reference).await?;
                    if (!key.starts_with(&node.prefix) && value.is_none())
                        || (key == node.prefix && node.value == value)
                    {
                        return Ok(Some(Pending::Stored(reference)));
                    }
                    (
                        PendingNode {
                            prefix: node.prefix.clone(),
                            value: node.value.clone(),
                            children: node
                                .children
                                .iter()
                                .map(|(k, v)| (*k, Pending::Stored(v.clone())))
                                .collect(),
                        },
                        Some(reference),
                    )
                }
                Pending::Dirty(node) => (node, None),
            };
            let common = key
                .bytes()
                .zip(node.prefix.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            if common < node.prefix.len() {
                let Some(value) = value else {
                    return Ok(Some(Pending::Dirty(node)));
                };
                let mut parent = PendingNode {
                    prefix: key[..common].into(),
                    value: None,
                    children: BTreeMap::new(),
                };
                parent.children.insert(
                    node.prefix.as_bytes()[common] as char,
                    original
                        .map(Pending::Stored)
                        .unwrap_or(Pending::Dirty(node)),
                );
                if common == key.len() {
                    parent.value = Some(value);
                } else {
                    parent.children.insert(
                        key.as_bytes()[common] as char,
                        Pending::Dirty(PendingNode {
                            prefix: key.into(),
                            value: Some(value),
                            children: BTreeMap::new(),
                        }),
                    );
                }
                return Ok(Some(Pending::Dirty(parent)));
            }
            if key == node.prefix {
                node.value = value;
            } else {
                let c = key.as_bytes()[node.prefix.len()] as char;
                if let Some(child) = self
                    .edit(node.children.remove(&c), key, value, ctx, depth + 1)
                    .await?
                {
                    node.children.insert(c, child);
                }
            }
            if node.value.is_none() {
                if node.children.is_empty() {
                    return Ok(None);
                }
                if node.children.len() == 1 {
                    return Ok(node.children.into_values().next());
                }
            }
            Ok(Some(Pending::Dirty(node)))
        })
    }
    fn flush<'a>(
        &'a self,
        node: Pending,
        inc: &'a str,
        epoch: u64,
        category: &'a str,
        ctx: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Result<ObjectRef>> + Send + 'a>> {
        Box::pin(async move {
            ctx.check()?;
            match node {
                Pending::Stored(reference) => Ok(reference),
                Pending::Dirty(node) => {
                    let mut children = BTreeMap::new();
                    for (key, child) in node.children {
                        children.insert(key, self.flush(child, inc, epoch, category, ctx).await?);
                    }
                    self.put(
                        inc,
                        epoch,
                        category,
                        &IndexNode {
                            version: FORMAT_VERSION,
                            prefix: node.prefix,
                            value: node.value,
                            children,
                        },
                        ctx,
                    )
                    .await
                }
            }
        })
    }
}
