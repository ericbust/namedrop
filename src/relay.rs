//! The relay: a mention to an agent on another machine goes from this
//! Namedrop to the Namedrop beside that machine's Rex server, which delivers
//! it there. Replies come back by this side's pull.

#[cfg(test)]
mod store;
