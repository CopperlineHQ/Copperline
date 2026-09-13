# Microsoft Store package identity for Copperline.
#
# These three values are issued by Partner Center and must be copied from
# "Product identity" on the app's Product management page EXACTLY as shown.
# They are case-sensitive, and a mismatch in any of them makes Partner Center
# reject the upload with an identity error rather than a useful message.
#
# They are committed rather than kept as CI secrets on purpose: none of them
# is a credential. The package identity is public in every installed copy,
# and the signature that makes a package trustworthy is applied by the Store,
# not by anything in this repository.
@{
    # Package/Identity/Name. Partner Center derives this from the reserved
    # app name and the publisher prefix on the account.
    Name = "LinuxJedi.Copperline"

    # Package/Identity/Publisher: the account's publisher ID, which is a
    # subject name rather than a display name.
    Publisher = "CN=C725AB56-85DF-4409-88E9-573323598917"

    # Package/Properties/PublisherDisplayName: the name shown to customers
    # in the Store listing.
    PublisherDisplayName = "LinuxJedi"
}
