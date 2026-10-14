//! `has_one`, and relations over nullable keys (`one_of`): a namespace belongs to a user or
//! an organization, never both, and each of them has one.

use kiln::prelude::*;
use namespace::{Namespace, NewNamespace};
use org::{NewOrg, Org, OrgRelations};
use person::{NewPerson, Person, PersonRelations};
use star::{NewStar, Star};

mod person {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "people", has_one(namespace: Namespace), has_many(stars: Star))]
    pub struct Person {
        pub id: Id<Person>,
        pub name: String,
    }
}

mod org {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "orgs", has_one(namespace: Namespace))]
    pub struct Org {
        pub id: Id<Org>,
        pub name: String,
    }
}

mod namespace {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "namespaces", belongs_to(person: Person, org: Org), one_of(person_id, org_id))]
    pub struct Namespace {
        pub id: Id<Namespace>,
        #[index(unique)]
        pub path: String,
        #[on_delete(cascade)]
        #[index(unique)]
        pub person_id: Option<Id<Person>>,
        #[on_delete(cascade)]
        #[index(unique)]
        pub org_id: Option<Id<Org>>,
    }
}

mod star {
    use super::*;

    /// A star from a person, or from nobody once they're gone.
    #[derive(Record, Debug, Clone)]
    #[record(table = "stars", belongs_to(person: Person))]
    pub struct Star {
        pub id: Id<Star>,
        #[on_delete(set_null)]
        pub person_id: Option<Id<Person>>,
    }
}

async fn db() -> Result<Db> {
    let db = Db::memory().await?;
    for schema in [Person::schema(), Org::schema(), Namespace::schema(), Star::schema()] {
        for statement in schema.create_sql() {
            db.execute(&statement).await?;
        }
    }
    Ok(db)
}

#[tokio::test]
async fn each_owner_has_one_namespace() -> Result {
    let db = db().await?;
    let ada = Person::insert(NewPerson { name: "Ada".into() }).run(&db).await?;
    let grace = Person::insert(NewPerson { name: "Grace".into() }).run(&db).await?;
    let org = Org::insert(NewOrg { name: "Engines".into() }).run(&db).await?;
    Namespace::insert(NewNamespace { path: "ada".into(), person_id: Some(ada.id), org_id: None }).run(&db).await?;
    Namespace::insert(NewNamespace { path: "engines".into(), person_id: None, org_id: Some(org.id) }).run(&db).await?;

    let people = Person::query().preload(person::namespace).order(person::id.asc()).all(&db).await?;
    assert_eq!(people[0].namespace().as_ref().map(|n| n.path.as_str()), Some("ada"));
    assert!(people[1].namespace().is_none(), "Grace has none yet");
    let orgs = Org::query().preload(org::namespace).all(&db).await?;
    assert_eq!(orgs[0].namespace().as_ref().map(|n| n.path.as_str()), Some("engines"));

    // `one_of`: exactly one owner.
    let both = NewNamespace { path: "both".into(), person_id: Some(grace.id), org_id: Some(org.id) };
    assert!(Namespace::insert(both).run(&db).await.is_err());
    let neither = NewNamespace { path: "neither".into(), person_id: None, org_id: None };
    assert!(Namespace::insert(neither).run(&db).await.is_err());

    // Deleting the owner takes its namespace (cascade) and keeps its stars, unlinked (set null).
    Star::insert(NewStar { person_id: Some(ada.id) }).run(&db).await?;
    let ada_loaded = Person::find(ada.id).preload(person::stars).one_or_404(&db).await?;
    assert_eq!(ada_loaded.stars().len(), 1);
    Person::delete(ada.id).run(&db).await?;
    assert!(!Namespace::query().filter(namespace::path.eq("ada")).exists(&db).await?);
    assert_eq!(Star::query().filter(star::person_id.is_null()).count(&db).await?, 1);
    Ok(())
}
