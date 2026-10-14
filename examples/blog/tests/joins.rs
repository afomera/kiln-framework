//! Typed joins and `has_many(.., through = ..)`, on a slice of a code host: people in
//! organizations, repositories in namespaces, labelled issues. Runs on Postgres too with
//! `KILN_TEST_DATABASE_URL`.

use issue::{Issue, IssueRelations, NewIssue};
use kiln::{prelude::*, testing::TestApp};
use label::{Label, NewLabel};
use labeling::{Labeling, NewLabeling};
use membership::{Membership, NewMembership};
use namespace::{Namespace, NewNamespace};
use organization::{NewOrganization, Organization, OrganizationRelations};
use person::{NewPerson, Person, PersonRelations};
use profile::{NewProfile, Profile};
use repository::{NewRepository, Repository};

mod person {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(
        table = "people",
        has_one(profile: Profile),
        has_many(memberships: Membership, organizations: Organization, through = memberships)
    )]
    pub struct Person {
        pub id: Id<Person>,
        pub name: String,
    }
}

mod profile {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "profiles", belongs_to(person: Person))]
    pub struct Profile {
        pub id: Id<Profile>,
        #[index(unique)]
        pub person_id: Id<Person>,
        pub bio: String,
    }
}

mod organization {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "organizations", has_many(memberships: Membership, members: Person, through = memberships))]
    pub struct Organization {
        pub id: Id<Organization>,
        pub name: String,
    }
}

mod membership {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "memberships", belongs_to(organization: Organization, person: Person))]
    pub struct Membership {
        pub id: Id<Membership>,
        pub organization_id: Id<Organization>,
        pub person_id: Id<Person>,
        pub role: String,
    }
}

mod namespace {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "namespaces")]
    pub struct Namespace {
        pub id: Id<Namespace>,
        pub path: String,
    }
}

mod repository {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "repositories", belongs_to(namespace: Namespace, maintainer: Person))]
    pub struct Repository {
        pub id: Id<Repository>,
        pub name: String,
        pub namespace_id: Id<Namespace>,
        pub maintainer_id: Option<Id<Person>>,
        pub stars: i64,
    }
}

mod issue {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(
        table = "issues",
        belongs_to(repository: Repository, author: Person),
        has_many(labelings: Labeling, labels: Label, through = labelings)
    )]
    pub struct Issue {
        pub id: Id<Issue>,
        pub title: String,
        pub repository_id: Id<Repository>,
        pub author_id: Id<Person>,
    }
}

mod label {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "labels")]
    pub struct Label {
        pub id: Id<Label>,
        pub name: String,
    }
}

mod labeling {
    use super::*;

    #[derive(Record, Debug, Clone)]
    #[record(table = "labelings", belongs_to(issue: Issue, label: Label))]
    pub struct Labeling {
        pub id: Id<Labeling>,
        #[on_delete(cascade)]
        pub issue_id: Id<Issue>,
        pub label_id: Id<Label>,
    }
}

/// The blog's migrations don't know these tables; on Postgres the test schema already
/// has them (it's built from every record in the binary).
async fn create_tables(db: &Db) -> Result {
    let schemas = [
        Person::schema(),
        Profile::schema(),
        Organization::schema(),
        Membership::schema(),
        Namespace::schema(),
        Repository::schema(),
        Issue::schema(),
        Label::schema(),
        Labeling::schema(),
    ];
    for schema in schemas {
        let columns: Vec<String> = db.sql(db.dialect().columns_query(schema.table)).all().await?;
        if columns.is_empty() {
            for statement in schema.create_sql_for(db.dialect()) {
                db.execute(&statement).await?;
            }
        }
    }
    Ok(())
}

struct Forge {
    ada: Person,
    bob: Person,
    afomera: Namespace,
    rails: Namespace,
}

async fn forge(db: &Db) -> Result<Forge> {
    create_tables(db).await?;
    let ada = Person::insert(NewPerson { name: "Ada".into() }).run(db).await?;
    let bob = Person::insert(NewPerson { name: "Bob".into() }).run(db).await?;
    let afomera = Namespace::insert(NewNamespace { path: "afomera".into() }).run(db).await?;
    let rails = Namespace::insert(NewNamespace { path: "rails".into() }).run(db).await?;
    for (name, namespace, maintainer, stars) in [
        ("kiln", &afomera, Some(bob.id), 10),
        ("rails", &rails, Some(ada.id), 50),
        ("forked", &afomera, None, 5),
        ("arel", &rails, None, 20),
    ] {
        let new = NewRepository { name: name.into(), namespace_id: namespace.id, maintainer_id: maintainer, stars };
        Repository::insert(new).run(db).await?;
    }
    Ok(Forge { ada, bob, afomera, rails })
}

fn names(repos: &[Repository]) -> Vec<&str> {
    repos.iter().map(|r| r.name.as_str()).collect()
}

#[kiln::test]
async fn joins_filter_and_order_by_the_joined_table(app: TestApp) -> Result {
    let db = &app.db;
    let f = forge(db).await?;

    // Filter and order through a belongs_to; rows stay `Repository`.
    let in_afomera = Repository::query()
        .join(repository::namespace)
        .filter(namespace::path.eq("afomera"))
        .order(repository::name.asc())
        .all(db)
        .await?;
    assert_eq!(names(&in_afomera), ["forked", "kiln"]);

    let by_path = Repository::query()
        .join(repository::namespace)
        .order(namespace::path.desc())
        .order(repository::stars.desc())
        .all(db)
        .await?;
    assert_eq!(names(&by_path), ["rails", "arel", "kiln", "forked"]);

    // Both tables have `id` and `name`-like columns; every column is qualified.
    let one = Repository::query()
        .join(repository::namespace)
        .filter(namespace::id.eq(f.rails.id))
        .filter(repository::id.gt(Id::new(0)))
        .order(repository::id.asc())
        .one_or_404(db)
        .await?;
    assert_eq!(one.name, "rails");

    // An optional belongs_to: `join` drops rows without one, `left_join` keeps them.
    let maintained = Repository::query().join(repository::maintainer).order(person::name.asc()).all(db).await?;
    assert_eq!(names(&maintained), ["rails", "kiln"]);
    let all = Repository::query().left_join(repository::maintainer).order(repository::name.asc()).count(db).await?;
    assert_eq!(all, 4);
    let unmaintained = Repository::query()
        .left_join(repository::maintainer)
        .filter(person::name.ne("Ada"))
        .select(repository::name)
        .all(db)
        .await?;
    assert_eq!(unmaintained, ["kiln"], "a filter on a missing left-joined row doesn't match");

    // Chains: a relation of a joined table.
    let mk = |title: &str, repo: &str, author: &Person| {
        let (title, repo, author) = (title.to_owned(), repo.to_owned(), author.id);
        async move {
            let repository = Repository::query().filter(repository::name.eq(repo)).one_or_404(db).await?;
            Issue::insert(NewIssue { title, repository_id: repository.id, author_id: author }).run(db).await
        }
    };
    mk("Typed joins", "kiln", &f.ada).await?;
    mk("Has many through", "kiln", &f.bob).await?;
    mk("Active Record", "rails", &f.bob).await?;
    let issues = Issue::query()
        .join(issue::repository)
        .join(repository::namespace)
        .join(issue::author)
        .filter(namespace::path.eq("afomera"))
        .order(person::name.asc())
        .preload(issue::author)
        .all(db)
        .await?;
    let titles: Vec<(&str, &str)> = issues.iter().map(|i| (i.title.as_str(), i.author().name.as_str())).collect();
    assert_eq!(titles, [("Typed joins", "Ada"), ("Has many through", "Bob")]);

    // Pluck and aggregate the joined table.
    let paths = Issue::query()
        .join(issue::repository)
        .join(repository::namespace)
        .order(issue::id.asc())
        .select(namespace::path)
        .all(db)
        .await?;
    assert_eq!(paths, ["afomera", "afomera", "rails"]);
    let per_path = Repository::query().join(repository::namespace).count_by(namespace::path, db).await?;
    assert_eq!(per_path, [("afomera".to_owned(), 2), ("rails".to_owned(), 2)]);
    let stars = Issue::query().join(issue::repository).filter(issue::author_id.eq(f.bob.id));
    assert_eq!(stars.clone().sum(repository::stars, db).await?, 60);
    assert_eq!(stars.max(repository::stars, db).await?, Some(50));

    // A joined query is a subquery like any other.
    let in_rails = Issue::query().filter(
        issue::repository_id
            .in_query(Repository::query().join(repository::namespace).filter(namespace::path.eq("rails"))),
    );
    assert_eq!(in_rails.count(db).await?, 1);
    assert!(Repository::query().join(repository::namespace).filter(namespace::id.eq(f.afomera.id)).exists(db).await?);
    Ok(())
}

#[kiln::test]
async fn joined_deletes_locks_and_has_one(app: TestApp) -> Result {
    let db = &app.db;
    let f = forge(db).await?;
    Profile::insert(NewProfile { person_id: f.ada.id, bio: "Engines".into() }).run(db).await?;

    // has_one joins on the child's key.
    let with_profile = Person::query().join(person::profile).filter(profile::bio.like("Eng%")).all(db).await?;
    assert_eq!(with_profile.len(), 1);
    assert_eq!(with_profile[0].name, "Ada");
    let everyone = Person::query().left_join(person::profile).order(profile::bio.asc()).count(db).await?;
    assert_eq!(everyone, 2);

    // Locks the base table's rows only, inside a transaction.
    let locked = db
        .transaction(|tx| async move {
            Repository::query()
                .left_join(repository::maintainer)
                .filter(person::name.eq("Ada"))
                .for_update()
                .all(&tx)
                .await
        })
        .await?;
    assert_eq!(names(&locked), ["rails"]);

    // Deletes go through a subquery and only touch the base table.
    let gone =
        Repository::query().join(repository::namespace).filter(namespace::path.eq("rails")).delete_all(db).await?;
    assert_eq!(gone, 2);
    assert_eq!(Repository::query().count(db).await?, 2);
    assert_eq!(Namespace::query().count(db).await?, 2);

    // A table can't be joined twice: every column of it would be ambiguous.
    let twice = Issue::query().join(issue::author).join(issue::repository).join(repository::maintainer);
    let err = twice.count(db).await.unwrap_err();
    assert!(err.to_string().contains("joins `people` twice"), "{err}");
    Ok(())
}

#[kiln::test]
async fn has_many_through_preloads_in_two_queries(app: TestApp) -> Result {
    let db = &app.db;
    let f = forge(db).await?;
    let carol = Person::insert(NewPerson { name: "Carol".into() }).run(db).await?;
    let engines = Organization::insert(NewOrganization { name: "Engines".into() }).run(db).await?;
    let looms = Organization::insert(NewOrganization { name: "Looms".into() }).run(db).await?;
    let empty = Organization::insert(NewOrganization { name: "Empty".into() }).run(db).await?;
    for (org, person, role) in [(&engines, &carol, "owner"), (&engines, &f.ada, "member"), (&looms, &f.ada, "owner")] {
        let new = NewMembership { organization_id: org.id, person_id: person.id, role: role.into() };
        Membership::insert(new).run(db).await?;
    }
    Profile::insert(NewProfile { person_id: f.ada.id, bio: "Engines".into() }).run(db).await?;

    let before = db.query_count();
    let orgs = Organization::query()
        .preload(organization::members.preload(person::profile))
        .order(organization::id.asc())
        .all(db)
        .await?;
    // The orgs, their memberships, the people, the people's profiles.
    assert_eq!(db.query_count() - before, 4);
    let members: Vec<Vec<&str>> = orgs.iter().map(|o| o.members().iter().map(|p| p.name.as_str()).collect()).collect();
    assert_eq!(members, [vec!["Carol", "Ada"], vec!["Ada"], vec![]]);
    assert_eq!(orgs[0].members()[1].profile().as_ref().map(|p| p.bio.as_str()), Some("Engines"));
    assert!(orgs[2].members().is_empty() && orgs[2].id == empty.id);

    // The other direction, and alongside the plain relation it goes through.
    let ada = Person::find(f.ada.id).preload(person::organizations).preload(person::memberships).one_or_404(db).await?;
    let org_names: Vec<&str> = ada.organizations().iter().map(|o| o.name.as_str()).collect();
    assert_eq!(org_names, ["Engines", "Looms"]);
    assert_eq!(ada.memberships().len(), 2);

    // Serialized like any relation.
    let json = serde_json::to_value(&orgs[1]).expect("serializes");
    assert_eq!(json["members"][0]["name"], "Ada");

    // Labels through labelings.
    let repo = Repository::query().one_or_404(db).await?;
    let new_issue = |title: &str| NewIssue { title: title.into(), repository_id: repo.id, author_id: f.bob.id };
    let bug = Issue::insert(new_issue("Crash")).run(db).await?;
    Issue::insert(new_issue("Idea")).run(db).await?;
    for name in ["bug", "p1"] {
        let label = Label::insert(NewLabel { name: name.into() }).run(db).await?;
        Labeling::insert(NewLabeling { issue_id: bug.id, label_id: label.id }).run(db).await?;
    }
    let issues = Issue::query().preload(issue::labels).order(issue::id.asc()).all(db).await?;
    let labels: Vec<Vec<&str>> = issues.iter().map(|i| i.labels().iter().map(|l| l.name.as_str()).collect()).collect();
    assert_eq!(labels, [vec!["bug", "p1"], vec![]]);
    Ok(())
}
