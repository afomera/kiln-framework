// The join record must point at the target: `Membership.person_id` here is a `Team` key.
use kiln::prelude::*;

mod team {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "teams", has_many(memberships: Membership, members: Person, through = memberships))]
    pub struct Team {
        pub id: Id<Team>,
    }
}

mod membership {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "memberships")]
    pub struct Membership {
        pub id: Id<Membership>,
        pub team_id: Id<Team>,
        pub person_id: Id<Team>,
    }
}

mod person {
    #[derive(kiln::Record, Debug, Clone)]
    #[record(table = "people")]
    pub struct Person {
        pub id: kiln::Id<Person>,
    }
}

use membership::Membership;
use person::Person;
use team::Team;

fn main() {}
