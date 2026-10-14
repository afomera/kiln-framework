//! Deny by default: an API route that signs someone in names the token scope it needs, so a
//! narrow token can't reach a route whose author forgot to check.

use blog::records::*;
use kiln::{
    prelude::*,
    testing::{Method, TestApp},
};

mod actions {
    pub mod api_scopes {
        pub mod whoami {
            use blog::records::User;
            use kiln::prelude::*;

            #[action(GET "/api/test/whoami", pipeline = Api, scope = "any")]
            pub async fn whoami(user: Current<User>) -> Result<String> {
                Ok(user.name.clone())
            }
        }

        pub mod destroy {
            use blog::records::User;
            use kiln::prelude::*;

            #[action(DELETE "/api/test/everything", pipeline = Api, scope = "admin")]
            pub async fn destroy(user: Current<User>) -> Result<String> {
                Ok(format!("{} deleted everything", user.name))
            }
        }
    }
}

use actions::api_scopes::{destroy, whoami};

#[kiln::test]
async fn routes_take_the_tokens_their_scope_allows(app: TestApp) -> Result {
    let user = app.create_with::<User>(|u| u.name = "Ada".into()).await?;
    // `any`: every token, even one with no scopes.
    app.api_client(&user, &[]).await?.get(whoami::path()).await.assert_status(200).assert_contains("Ada");

    let reader = app.api_client(&user, &["read"]).await?;
    let refused = reader.request(Method::DELETE, &destroy::path(), None).await;
    refused.assert_status(403);
    assert_eq!(refused.json()["code"], "K1021");
    assert!(refused.json()["detail"].as_str().unwrap().contains("`admin`"));

    let admin = app.api_client(&user, &["admin"]).await?;
    admin.request(Method::DELETE, &destroy::path(), None).await.assert_status(200);
    Ok(())
}
