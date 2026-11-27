"""The errors strkit raises."""


class StrkitError(ValueError):
    """The base of every error strkit raises."""


class DurationError(StrkitError):
    pass


class RomanError(StrkitError):
    pass


class VersionError(StrkitError):
    pass


class TemplateError(StrkitError):
    pass
