module Ledger
  class LegacyInvoice < ApplicationRecord
    self.table_name = "documents"
  end
end
