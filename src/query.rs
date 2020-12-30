use fnv::{FnvHashMap, FnvHashSet};
use libipld::cid::Cid;
use libp2p::PeerId;
use std::collections::VecDeque;

/// Query id.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct QueryId(u64);

impl std::fmt::Display for QueryId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Query type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryType {
    Get,
    Sync,
}

/// Request.
#[derive(Debug, Eq, PartialEq)]
pub enum Request {
    /// Have query.
    Have(PeerId, Cid),
    /// Block query.
    Block(PeerId, Cid),
    /// Providers query.
    Providers(Cid),
    /// Missing blocks query.
    MissingBlocks(Cid),
}

impl std::fmt::Display for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Have(_, _) => write!(f, "have"),
            Self::Block(_, _) => write!(f, "block"),
            Self::Providers(_) => write!(f, "providers"),
            Self::MissingBlocks(_) => write!(f, "missing-blocks"),
        }
    }
}

/// Response.
#[derive(Debug)]
pub enum Response {
    /// Have query.
    Have(PeerId, bool),
    /// Block query.
    Block(PeerId, bool),
    /// Providers query.
    Providers(Vec<PeerId>),
    /// Missing blocks query.
    MissingBlocks(Vec<Cid>),
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Have(_, have) => write!(f, "have {}", have),
            Self::Block(_, block) => write!(f, "block {}", block),
            Self::Providers(providers) => write!(f, "providers {}", providers.len()),
            Self::MissingBlocks(missing) => write!(f, "missing-blocks {}", missing.len()),
        }
    }
}

/// Event emitted by a query.
#[derive(Debug)]
pub enum QueryEvent {
    /// A subquery to run.
    Request(QueryId, Request),
    /// Complete event.
    Complete(QueryId, QueryType, Result<(), Cid>),
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    /// Query id.
    pub id: QueryId,
    /// Root query id.
    pub root: QueryId,
    /// Parent.
    pub parent: Option<QueryId>,
    /// Cid.
    pub cid: Cid,
}

/// Query.
#[derive(Debug)]
struct Query {
    /// Header.
    hdr: Header,
    /// State.
    state: State,
}

#[derive(Debug)]
enum State {
    None,
    Get(GetState),
    Sync(SyncState),
}

#[derive(Debug, Default)]
struct GetState {
    initial: bool,
    have: FnvHashSet<QueryId>,
    block: Option<QueryId>,
    res: Vec<PeerId>,
}

#[derive(Debug, Default)]
struct SyncState {
    missing: FnvHashSet<QueryId>,
    children: FnvHashMap<QueryId, Vec<PeerId>>,
}

enum Transition<S, C> {
    Next(S),
    Complete(C),
}

#[derive(Default)]
pub struct QueryManager {
    id_counter: u64,
    queries: FnvHashMap<QueryId, Query>,
    events: VecDeque<QueryEvent>,
}

impl QueryManager {
    /// Start a new subquery.
    fn start_query(
        &mut self,
        root: QueryId,
        parent: Option<QueryId>,
        cid: Cid,
        req: Request,
    ) -> QueryId {
        let id = QueryId(self.id_counter);
        self.id_counter += 1;
        let query = Query {
            hdr: Header {
                id,
                root,
                parent,
                cid,
            },
            state: State::None,
        };
        self.queries.insert(id, query);
        tracing::trace!("{} {} {}", root, id, req);
        self.events.push_back(QueryEvent::Request(id, req));
        id
    }

    /// Starts a new have query to ask a peer if it has a block.
    fn have(&mut self, root: QueryId, parent: QueryId, peer_id: PeerId, cid: Cid) -> QueryId {
        self.start_query(root, Some(parent), cid, Request::Have(peer_id, cid))
    }

    /// Starts a new block query to request a block from a peer.
    fn block(&mut self, root: QueryId, parent: QueryId, peer_id: PeerId, cid: Cid) -> QueryId {
        self.start_query(root, Some(parent), cid, Request::Block(peer_id, cid))
    }

    /// Starts a query to determine the providers of a block.
    fn providers(&mut self, root: QueryId, parent: QueryId, cid: Cid) -> QueryId {
        self.start_query(root, Some(parent), cid, Request::Providers(cid))
    }

    /// Starts a query to determine the missing blocks of a dag.
    fn missing_blocks(&mut self, parent: QueryId, cid: Cid) -> QueryId {
        self.start_query(parent, Some(parent), cid, Request::MissingBlocks(cid))
    }

    /// Starts a query to locate and retrieve a block. The initial peers will be queried first
    /// before performing a more expensive provider query.
    pub fn get(
        &mut self,
        parent: Option<QueryId>,
        cid: Cid,
        initial: impl Iterator<Item = PeerId>,
    ) -> QueryId {
        let id = QueryId(self.id_counter);
        self.id_counter += 1;
        let root = parent.unwrap_or(id);
        tracing::trace!("{} {} get", root, id);
        let mut state = GetState::default();
        state.initial = true;
        for peer in initial {
            if state.block.is_none() {
                state.block = Some(self.block(root, id, peer, cid));
            } else {
                state.have.insert(self.have(root, id, peer, cid));
            }
        }
        if state.block.is_none() {
            self.providers(root, id, cid);
        }
        let query = Query {
            hdr: Header {
                id,
                root,
                parent,
                cid,
            },
            state: State::Get(state),
        };
        self.queries.insert(id, query);
        id
    }

    /// Starts a query to recursively retrieve a dag. The missing blocks are the first
    /// blocks that need to be retrieved.
    pub fn sync(&mut self, cid: Cid, missing: impl Iterator<Item = Cid>) -> QueryId {
        let id = QueryId(self.id_counter);
        self.id_counter += 1;
        tracing::trace!("{} {} sync", id, id);
        let mut state = SyncState::default();
        for cid in missing {
            state
                .missing
                .insert(self.get(Some(id), cid, std::iter::empty()));
        }
        if state.missing.is_empty() {
            state.children.insert(self.missing_blocks(id, cid), vec![]);
        }
        let query = Query {
            hdr: Header {
                id,
                root: id,
                parent: None,
                cid,
            },
            state: State::Sync(state),
        };
        self.queries.insert(id, query);
        id
    }

    /// Cancels an in progress query.
    pub fn cancel(&mut self, root: QueryId) -> Option<QueryType> {
        let query = self.queries.remove(&root)?;
        let queries = &self.queries;
        self.events.retain(|event| {
            let (id, req) = match event {
                QueryEvent::Request(id, req) => (id, req),
                QueryEvent::Complete(_, _, _) => return true,
            };
            if queries.get(id).map(|q| q.hdr.root) != Some(root) {
                return true;
            }
            tracing::trace!("{} {} {} cancel", root, id, req);
            false
        });
        match query.state {
            State::Get(_) => {
                tracing::trace!("{} {} get cancel", root, root);
                Some(QueryType::Get)
            }
            State::Sync(state) => {
                for id in state.missing {
                    tracing::trace!("{} {} get cancel", root, id);
                    self.queries.remove(&id);
                }
                tracing::trace!("{} {} sync cancel", root, root);
                Some(QueryType::Sync)
            }
            State::None => {
                self.queries.insert(root, query);
                None
            }
        }
    }

    /// Advances a get query state machine using a transition function.
    fn get_query<F>(&mut self, id: QueryId, f: F)
    where
        F: FnOnce(&mut Self, &Header, GetState) -> Transition<GetState, Vec<PeerId>>,
    {
        if let Some(mut parent) = self.queries.remove(&id) {
            let state = if let State::Get(state) = parent.state {
                state
            } else {
                return;
            };
            match f(self, &parent.hdr, state) {
                Transition::Next(state) => {
                    parent.state = State::Get(state);
                    self.queries.insert(id, parent);
                }
                Transition::Complete(providers) => {
                    if providers.is_empty() {
                        tracing::trace!("{} {} get err", parent.hdr.root, parent.hdr.id);
                    } else {
                        tracing::trace!("{} {} get ok", parent.hdr.root, parent.hdr.id);
                    }
                    self.recv_get(parent.hdr, providers);
                }
            }
        }
    }

    /// Advances a sync query state machine using a transition function.
    fn sync_query<F>(&mut self, id: QueryId, f: F)
    where
        F: FnOnce(&mut Self, &Header, SyncState) -> Transition<SyncState, Result<(), Cid>>,
    {
        if let Some(mut parent) = self.queries.remove(&id) {
            let state = if let State::Sync(state) = parent.state {
                state
            } else {
                return;
            };
            match f(self, &parent.hdr, state) {
                Transition::Next(state) => {
                    parent.state = State::Sync(state);
                    self.queries.insert(id, parent);
                }
                Transition::Complete(res) => {
                    if res.is_ok() {
                        tracing::trace!("{} {} sync ok", parent.hdr.root, parent.hdr.id);
                    } else {
                        tracing::trace!("{} {} sync err", parent.hdr.root, parent.hdr.id);
                    }
                    self.recv_sync(parent.hdr, res);
                }
            }
        }
    }

    /// Processes the response of a have query.
    ///
    /// Marks the in progress query as complete and updates the set of peers that have
    /// a block. If there isn't an in progress block query a new block query will be
    /// started. If no block query can be started either a provider query is started or
    /// the get query is marked as complete with a block-not-found error.
    fn recv_have(&mut self, query: Header, peer_id: PeerId, have: bool) {
        self.get_query(query.parent.unwrap(), |mgr, parent, mut state| {
            state.have.remove(&query.id);
            if state.block == Some(query.id) {
                state.block = None;
            }
            if have {
                state.res.push(peer_id);
            }
            if state.block.is_none() && !state.res.is_empty() {
                state.block =
                    Some(mgr.block(parent.root, parent.id, state.res.pop().unwrap(), query.cid));
            }
            if state.have.is_empty() && state.block.is_none() && state.res.is_empty() {
                if state.initial {
                    mgr.providers(parent.root, parent.id, query.cid);
                } else {
                    return Transition::Complete(state.res);
                }
            }
            Transition::Next(state)
        });
    }

    /// Processes the response of a block query.
    ///
    /// Either completes the get query or processes it like a have query response.
    fn recv_block(&mut self, query: Header, peer_id: PeerId, block: bool) {
        if block {
            self.get_query(query.parent.unwrap(), |_mgr, _parent, mut state| {
                state.res.push(peer_id);
                Transition::Complete(state.res)
            });
        } else {
            self.recv_have(query, peer_id, block);
        }
    }

    /// Processes the response of a providers query.
    ///
    /// Either starts a new set of block and have queries or marks the get query
    /// as complete with a `block-not-found` error if no new providers where found.
    fn recv_providers(&mut self, query: Header, peers: Vec<PeerId>) {
        self.get_query(query.parent.unwrap(), |mgr, parent, mut state| {
            state.initial = false;
            for peer in peers {
                if state.block.is_none() {
                    state.block = Some(mgr.block(parent.root, parent.id, peer, query.cid));
                } else {
                    state
                        .have
                        .insert(mgr.have(parent.root, parent.id, peer, query.cid));
                }
            }
            if state.have.is_empty() && state.block.is_none() && state.res.is_empty() {
                Transition::Complete(state.res)
            } else {
                Transition::Next(state)
            }
        });
    }

    /// Processes the response of a missing blocks query.
    ///
    /// Starts a get query for each missing block. If there are no in progress queries
    /// the sync query is marked as complete.
    fn recv_missing_blocks(&mut self, query: Header, missing: Vec<Cid>) {
        self.sync_query(query.parent.unwrap(), |mgr, parent, mut state| {
            let providers = state.children.remove(&query.id).unwrap_or_default();
            for cid in missing {
                state
                    .missing
                    .insert(mgr.get(Some(parent.root), cid, providers.iter().copied()));
            }
            if state.missing.is_empty() && state.children.is_empty() {
                Transition::Complete(Ok(()))
            } else {
                Transition::Next(state)
            }
        });
    }

    /// Processes the response of a get query.
    ///
    /// If it is part of a sync query a new missing blocks query is started. Otherwise
    /// the get query emits a `complete` event.
    fn recv_get(&mut self, query: Header, providers: Vec<PeerId>) {
        if let Some(id) = query.parent {
            self.sync_query(id, |mgr, parent, mut state| {
                state.missing.remove(&query.id);
                if providers.is_empty() {
                    Transition::Complete(Err(query.cid))
                } else {
                    state
                        .children
                        .insert(mgr.missing_blocks(parent.root, query.cid), providers);
                    Transition::Next(state)
                }
            });
        } else {
            let res = if providers.is_empty() {
                Err(query.cid)
            } else {
                Ok(())
            };
            self.events
                .push_back(QueryEvent::Complete(query.id, QueryType::Get, res));
        }
    }

    /// Processes the response of a sync query.
    ///
    /// The sync query emits a `complete` event.
    fn recv_sync(&mut self, query: Header, res: Result<(), Cid>) {
        self.events
            .push_back(QueryEvent::Complete(query.id, QueryType::Sync, res));
    }

    /// Dispatches the response to a query handler.
    pub fn inject_response(&mut self, id: QueryId, res: Response) {
        let query = if let Some(query) = self.queries.remove(&id) {
            query.hdr
        } else {
            return;
        };
        tracing::trace!("{} {} {}", query.root, query.id, res);
        match res {
            Response::Have(peer, have) => {
                self.recv_have(query, peer, have);
            }
            Response::Block(peer, block) => {
                self.recv_block(query, peer, block);
            }
            Response::Providers(peers) => {
                self.recv_providers(query, peers);
            }
            Response::MissingBlocks(cids) => {
                self.recv_missing_blocks(query, cids);
            }
        }
    }

    /// Returns the header of a query.
    pub fn query_info(&self, id: QueryId) -> Option<&Header> {
        self.queries.get(&id).map(|q| &q.hdr)
    }

    /// Retrieves the next query event.
    pub fn next(&mut self) -> Option<QueryEvent> {
        self.events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_peers(n: usize) -> Vec<PeerId> {
        let mut peers = Vec::with_capacity(n);
        for _ in 0..n {
            peers.push(PeerId::random());
        }
        peers
    }

    fn assert_request(event: Option<QueryEvent>, req: Request) -> QueryId {
        if let Some(QueryEvent::Request(id, req2)) = event {
            assert_eq!(req2, req);
            id
        } else {
            panic!("{:?} is not a request", event);
        }
    }

    fn assert_complete(
        event: Option<QueryEvent>,
        id: QueryId,
        ty: QueryType,
        res: Result<(), Cid>,
    ) {
        if let Some(QueryEvent::Complete(id2, ty2, res2)) = event {
            assert_eq!(id, id2);
            assert_eq!(ty, ty2);
            assert_eq!(res, res2);
        } else {
            panic!("{:?} is not a complete event", event);
        }
    }

    #[test]
    fn test_get_query_block_not_found() {
        let mut mgr = QueryManager::default();
        let initial_set = gen_peers(3);
        let provider_set = gen_peers(3);
        let cid = Cid::default();

        let id = mgr.get(None, cid, initial_set.iter().copied());

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(initial_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(initial_set[2], cid));

        mgr.inject_response(id1, Response::Have(initial_set[0], false));
        mgr.inject_response(id2, Response::Have(initial_set[1], false));
        mgr.inject_response(id3, Response::Have(initial_set[2], false));

        let id1 = assert_request(mgr.next(), Request::Providers(cid));
        mgr.inject_response(id1, Response::Providers(provider_set.clone()));

        let id1 = assert_request(mgr.next(), Request::Block(provider_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(provider_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(provider_set[2], cid));

        mgr.inject_response(id1, Response::Have(provider_set[0], false));
        mgr.inject_response(id2, Response::Have(provider_set[1], false));
        mgr.inject_response(id3, Response::Have(provider_set[2], false));

        assert_complete(mgr.next(), id, QueryType::Get, Err(cid));
    }

    #[test]
    fn test_cid_query_block_initial_set() {
        let mut mgr = QueryManager::default();
        let initial_set = gen_peers(3);
        let cid = Cid::default();

        let id = mgr.get(None, cid, initial_set.iter().copied());

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(initial_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(initial_set[2], cid));

        mgr.inject_response(id1, Response::Block(initial_set[0], true));
        mgr.inject_response(id2, Response::Have(initial_set[1], false));
        mgr.inject_response(id3, Response::Have(initial_set[2], false));

        assert_complete(mgr.next(), id, QueryType::Get, Ok(()));
    }

    #[test]
    fn test_get_query_block_provider_set() {
        let mut mgr = QueryManager::default();
        let initial_set = gen_peers(3);
        let provider_set = gen_peers(3);
        let cid = Cid::default();

        let id = mgr.get(None, cid, initial_set.iter().copied());

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(initial_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(initial_set[2], cid));

        mgr.inject_response(id1, Response::Have(initial_set[0], false));
        mgr.inject_response(id2, Response::Have(initial_set[1], false));
        mgr.inject_response(id3, Response::Have(initial_set[2], false));

        let id1 = assert_request(mgr.next(), Request::Providers(cid));
        mgr.inject_response(id1, Response::Providers(provider_set.clone()));

        let id1 = assert_request(mgr.next(), Request::Block(provider_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(provider_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(provider_set[2], cid));

        mgr.inject_response(id1, Response::Block(provider_set[0], true));
        mgr.inject_response(id2, Response::Have(provider_set[1], false));
        mgr.inject_response(id3, Response::Have(provider_set[2], false));

        assert_complete(mgr.next(), id, QueryType::Get, Ok(()));
    }

    #[test]
    fn test_get_query_gets_from_spare_if_block_request_fails() {
        let mut mgr = QueryManager::default();
        let initial_set = gen_peers(3);
        let cid = Cid::default();

        let id = mgr.get(None, cid, initial_set.iter().copied());

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(initial_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(initial_set[2], cid));

        mgr.inject_response(id1, Response::Block(initial_set[0], false));
        mgr.inject_response(id2, Response::Have(initial_set[1], true));
        mgr.inject_response(id3, Response::Have(initial_set[2], false));

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[1], cid));
        mgr.inject_response(id1, Response::Block(initial_set[1], true));

        assert_complete(mgr.next(), id, QueryType::Get, Ok(()));
    }

    #[test]
    fn test_get_query_gets_from_spare_if_block_request_fails_after_have_is_received() {
        let mut mgr = QueryManager::default();
        let initial_set = gen_peers(3);
        let cid = Cid::default();

        let id = mgr.get(None, cid, initial_set.iter().copied());

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[0], cid));
        let id2 = assert_request(mgr.next(), Request::Have(initial_set[1], cid));
        let id3 = assert_request(mgr.next(), Request::Have(initial_set[2], cid));

        mgr.inject_response(id1, Response::Block(initial_set[0], false));
        mgr.inject_response(id2, Response::Have(initial_set[1], true));
        mgr.inject_response(id3, Response::Have(initial_set[2], true));

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[1], cid));
        mgr.inject_response(id1, Response::Block(initial_set[1], false));

        let id1 = assert_request(mgr.next(), Request::Block(initial_set[2], cid));
        mgr.inject_response(id1, Response::Block(initial_set[2], true));

        assert_complete(mgr.next(), id, QueryType::Get, Ok(()));
    }
}
