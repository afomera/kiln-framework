use kiln::prelude::*;

#[derive(Record, Debug, Clone)]
#[record(table = "subscriptions")]
pub struct Subscription {
    pub id: Id<Subscription>,
    #[index(unique)]
    pub email: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
